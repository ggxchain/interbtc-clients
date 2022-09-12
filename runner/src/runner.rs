use crate::{error::Error, Opts};
use backoff::{retry, Error as BackoffError, ExponentialBackoff};
use bytes::Bytes;
use codec::Decode;
use futures::{future::BoxFuture, FutureExt, StreamExt, TryFutureExt};
use nix::{
    sys::signal::{self, Signal},
    unistd::Pid,
};
use signal_hook_tokio::SignalsInfo;
use sp_core::{hexdisplay::AsBytesRef, H256};
use std::{
    convert::TryInto,
    fmt::{Debug, Display},
    fs::{self, OpenOptions},
    io::Write,
    os::unix::prelude::OpenOptionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    str,
    time::Duration,
};

use subxt::{dynamic::Value, OnlineClient, PolkadotConfig};

use async_trait::async_trait;

// TODO: Add client-type CLI argument.
/// Type of the client to run. One of `oracle`, `vault`, `faucet`.
/// Also used as the name of the downloaded executable.
pub const CLIENT_TYPE: &str = "vault";

/// Pallet in the parachain where the client release is assumed to be stored
pub const PARACHAIN_MODULE: &str = "ClientsInfo";

/// Storage item in the Pallet where the client release is assumed to be stored
pub const CURRENT_RELEASES_STORAGE_ITEM: &str = "CurrentClientReleases";

/// Parachain block time
pub const BLOCK_TIME: Duration = Duration::from_secs(6);

/// Timeout used by the retry utilities: One minute
pub const RETRY_TIMEOUT: Duration = Duration::from_millis(60_000);

/// Waiting interval used by the retry utilities: One minute
pub const RETRY_INTERVAL: Duration = Duration::from_millis(1_000);

/// Multiplier for the interval in retry utilities: Constant interval retry
pub const RETRY_MULTIPLIER: f64 = 1.0;

/// Data type assumed to be used by the parachain to store the client release.
/// If this type is different from the on-chain one, decoding will fail.
#[derive(Decode, Default, Eq, PartialEq, Debug, Clone)]
pub struct ClientRelease {
    /// A link to the Releases page of the `interbtc-clients` repo.
    /// Example: https://github.com/interlay/interbtc-clients/releases/download/1.16.0/vault-parachain-metadata-interlay
    pub uri: String,
    /// Code hash of the parachain runtime compatible with this release
    pub code_hash: H256,
}

/// Wrapper around `ClientRelease`, which includes details for running the executable.
#[derive(Default, Eq, PartialEq, Debug, Clone)]
pub struct DownloadedRelease {
    /// Release data read from the parachain
    pub release: ClientRelease,
    /// OS path where this release is stored
    pub path: PathBuf,
    /// Name of the executable
    pub bin_name: String,
}

/// Per-network manager of the vault executable
pub struct Runner {
    /// `subxt` api to the parachain
    subxt_api: OnlineClient<PolkadotConfig>,
    /// The child process (vault) spawned by this runner
    child_proc: Option<Child>,
    /// Details about the currently run release
    downloaded_release: Option<DownloadedRelease>,
    /// Runner CLI arguments
    opts: Opts,
}

impl Runner {
    pub fn new(subxt_api: OnlineClient<PolkadotConfig>, opts: Opts) -> Self {
        Self {
            subxt_api,
            child_proc: None,
            downloaded_release: None,
            opts,
        }
    }

    async fn download_binary(runner: &mut impl RunnerExt, release: ClientRelease) -> Result<DownloadedRelease, Error> {
        let (bin_name, bin_path) = runner.get_bin_path()?;
        log::info!("Downloading {} at: {:?}", bin_name, bin_path);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            // Make the binary executable.
            // The set permissions are: -rwx------
            .mode(0o700)
            .create(true)
            .open(bin_path.clone())?;

        let bytes = retry_with_log_async(
            || runner.get_request_bytes(release.uri.clone()).into_future().boxed(),
            "Error fetching executable".to_string(),
        )
        .await?;

        file.write_all(&bytes)?;
        file.sync_all()?;

        let downloaded_release = DownloadedRelease {
            release,
            path: bin_path,
            bin_name: bin_name.to_string(),
        };
        runner.set_downloaded_release(Some(downloaded_release.clone()));
        Ok(downloaded_release)
    }

    fn get_bin_path(runner: &impl RunnerExt) -> Result<(String, PathBuf), Error> {
        let bin_name = CLIENT_TYPE;
        let bin_path = runner.download_path().join(bin_name);
        Ok((bin_name.to_string(), bin_path))
    }

    fn delete_downloaded_release(runner: &mut impl RunnerExt) -> Result<(), Error> {
        let release = runner.downloaded_release().as_ref().ok_or(Error::NoDownloadedRelease)?;
        log::info!("Removing old release, with path {:?}", release.path);

        retry_with_log(
            || Ok(fs::remove_file(&release.path)?),
            "Failed to remove old release".to_string(),
        )?;

        runner.set_downloaded_release(None);
        Ok(())
    }

    fn terminate_proc_and_wait(runner: &mut impl RunnerExt) -> Result<u32, Error> {
        log::info!("Trying to terminate child process...");
        let child_proc = match runner.child_proc().as_mut() {
            Some(x) => x,
            None => {
                log::warn!("No child process to terminate.");
                return Ok(0);
            }
        };

        let _ = retry_with_log(
            || {
                Ok(signal::kill(
                    Pid::from_raw(child_proc.id().try_into().map_err(|_| Error::IntegerConversionError)?),
                    Signal::SIGTERM,
                ))
            },
            "Failed to kill child process".to_string(),
        )
        .map_err(|_| Error::ProcessTerminationFailure)?;

        match child_proc.wait() {
            Ok(exit_status) => log::info!(
                "Terminated vault process (pid: {}) with exit status {}",
                child_proc.id(),
                exit_status
            ),
            Err(error) => log::warn!("Vault process termination error: {}", error),
        };
        let pid = child_proc.id();
        runner.set_child_proc(None);
        Ok(pid)
    }

    async fn try_get_release<T: RunnerExt + StorageReader>(runner: &T) -> Result<Option<ClientRelease>, Error> {
        retry_with_log_async(
            || {
                runner
                    .read_chain_storage::<ClientRelease>(runner.subxt_api())
                    .into_future()
                    .boxed()
            },
            "Error fetching executable".to_string(),
        )
        .await
    }

    /// Read parachain storage via an RPC call, and decode the result
    async fn read_chain_storage<T: 'static + Decode + Debug>(
        subxt_api: &OnlineClient<PolkadotConfig>,
    ) -> Result<Option<T>, Error> {
        // Based on the implementation of `subxt_api.storage().fetch(...)`, but with decoding for a custom type. Source:
        // https://github.com/paritytech/subxt/blob/99cea97f817ee0a6fee642ff22f867822d9557f6/subxt/src/storage/storage_client.rs#L142
        let storage_address = subxt::dynamic::storage(
            PARACHAIN_MODULE,
            CURRENT_RELEASES_STORAGE_ITEM,
            vec![Value::from_bytes(CLIENT_TYPE.as_bytes())],
        );
        let lookup_bytes = subxt::storage::utils::storage_address_bytes(&storage_address, &subxt_api.metadata())?;
        let enc_res = subxt_api
            .storage()
            .fetch_raw(&lookup_bytes, None)
            .await?
            .map(Bytes::from);
        enc_res
            .map(|r| T::decode(&mut r.as_bytes_ref()))
            .transpose()
            .map_err(Into::into)
    }

    /// Run the auto-updater while concurrently listening for termination signals.
    pub async fn run(mut runner: Box<dyn RunnerExt + Send>, mut shutdown_signals: SignalsInfo) -> Result<(), Error> {
        tokio::select! {
            _ = shutdown_signals.next() => {
                runner.terminate_proc_and_wait()?;
            }
            result = runner.as_mut().auto_update() => {
                match result {
                    Ok(_) => log::error!("Auto-updater unexpectedly terminated."),
                    Err(e) => log::error!("Runner error: {}", e),
                }
                runner.terminate_proc_and_wait()?;
            }
        };
        Ok(())
    }

    async fn auto_update(runner: &mut impl RunnerExt) -> Result<(), Error> {
        // Create all directories for the `download_path` if they don't already exist.
        fs::create_dir_all(&runner.download_path())?;
        let release = runner
            .try_get_release()
            .await?
            .expect("No current client release set on-chain.");
        // WARNING: This will overwrite any pre-existing binary with the same name
        // TODO: Check if a release with the same version is already at the `download_path`
        runner.download_binary(release).await?;

        runner.run_binary()?;

        loop {
            runner.maybe_restart_client()?;
            if let Some(new_release) = runner.try_get_release().await? {
                let maybe_downloaded_release = runner.downloaded_release();
                let downloaded_release = maybe_downloaded_release.as_ref().ok_or(Error::NoDownloadedRelease)?;
                if new_release.uri != downloaded_release.release.uri {
                    // Wait for child process to finish completely.
                    // To ensure there can't be two vault processes using the same Bitcoin wallet.
                    runner.terminate_proc_and_wait()?;

                    // Delete old release
                    runner.delete_downloaded_release()?;

                    // Download new release
                    runner.download_binary(new_release).await?;

                    // Run the downloaded release
                    runner.run_binary()?;
                }
            }
            tokio::time::sleep(BLOCK_TIME).await;
        }
    }

    fn maybe_restart_client(runner: &mut impl RunnerExt) -> Result<(), Error> {
        if !runner.check_child_proc_alive()? {
            runner.run_binary()?;
        }
        Ok(())
    }

    fn check_child_proc_alive(runner: &mut impl RunnerExt) -> Result<bool, Error> {
        if let Some(child) = runner.child_proc() {
            // `try_wait` only returns if the child has already exited,
            // without actually doing any waiting
            match child.try_wait()? {
                Some(status) => {
                    log::info!("Child exited with: {status}");
                    runner.set_child_proc(None);
                    return Ok(false);
                }
                None => {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn run_binary(runner: &mut impl RunnerExt, stdout_mode: impl Into<Stdio>) -> Result<Child, Error> {
        if runner.child_proc().is_some() {
            return Err(Error::ChildProcessExists);
        }
        let downloaded_release = runner.downloaded_release().as_ref().ok_or(Error::NoDownloadedRelease)?;
        let mut command = Command::new(downloaded_release.path.as_os_str());
        command.args(runner.vault_args().clone()).stdout(stdout_mode);
        let child = retry_with_log(
            || command.spawn().map_err(Into::into),
            "Failed to spawn child process".to_string(),
        )?;
        log::info!("Client started, with pid {}", child.id());
        Ok(child)
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        if self
            .check_child_proc_alive()
            .expect("Failed to check child process status")
        {
            if let Err(e) = self.terminate_proc_and_wait() {
                log::warn!("Failed to terminate child process: {}", e);
            }
        }
    }
}

#[async_trait]
pub trait RunnerExt {
    fn subxt_api(&self) -> &OnlineClient<PolkadotConfig>;
    fn vault_args(&self) -> &Vec<String>;
    fn child_proc(&mut self) -> &mut Option<Child>;
    fn set_child_proc(&mut self, child_proc: Option<Child>);
    fn downloaded_release(&self) -> &Option<DownloadedRelease>;
    fn set_downloaded_release(&mut self, downloaded_release: Option<DownloadedRelease>);
    fn download_path(&self) -> &PathBuf;
    fn parachain_url(&self) -> String;
    /// Read the current client release from the parachain, retrying for `RETRY_TIMEOUT` if there is a network error.
    async fn try_get_release(&self) -> Result<Option<ClientRelease>, Error>;
    /// Download the vault binary and make it executable, retrying for `RETRY_TIMEOUT` if there is a network error.
    async fn download_binary(&mut self, release: ClientRelease) -> Result<(), Error>;
    /// Convert a release URI (e.g. a GitHub link) to an executable name and OS path (after download)
    fn get_bin_path(&self) -> Result<(String, PathBuf), Error>;
    /// Remove downloaded release from the file system. This is only supposed to occur _after_ the vault process
    /// has been killed. In case of failure, removing is retried for `RETRY_TIMEOUT`.
    fn delete_downloaded_release(&mut self) -> Result<(), Error>;
    /// Spawn a the client as a child process with the CLI arguments set in the `Runner`, retrying for
    /// `RETRY_TIMEOUT`.
    fn run_binary(&mut self) -> Result<(), Error>;
    /// Send a `SIGTERM` to the child process (via the underlying `kill` system call).
    /// If `kill` returns an error code, the operation is retried for `RETRY_TIMEOUT`.
    fn terminate_proc_and_wait(&mut self) -> Result<(), Error>;
    /// Get the client release executable, as `Bytes`
    async fn get_request_bytes(&self, url: String) -> Result<Bytes, Error>;
    /// Main loop, checks the parachain for new releases and updates the client accordingly.
    fn auto_update(&mut self) -> BoxFuture<'_, Result<(), Error>>;
    /// Returns whether the child is alive and sets the `runner.child` field to `None` if not.
    fn check_child_proc_alive(&mut self) -> Result<bool, Error>;
    /// If the child process crashed, start it again
    fn maybe_restart_client(&mut self) -> Result<(), Error>;
}

#[async_trait]
impl RunnerExt for Runner {
    fn subxt_api(&self) -> &OnlineClient<PolkadotConfig> {
        &self.subxt_api
    }

    fn vault_args(&self) -> &Vec<String> {
        &self.opts.vault_args
    }

    fn child_proc(&mut self) -> &mut Option<Child> {
        &mut self.child_proc
    }

    fn set_child_proc(&mut self, child_proc: Option<Child>) {
        self.child_proc = child_proc;
    }

    fn downloaded_release(&self) -> &Option<DownloadedRelease> {
        &self.downloaded_release
    }

    fn set_downloaded_release(&mut self, downloaded_release: Option<DownloadedRelease>) {
        self.downloaded_release = downloaded_release;
    }

    fn download_path(&self) -> &PathBuf {
        &self.opts.download_path
    }

    fn parachain_url(&self) -> String {
        self.opts.parachain_ws.clone()
    }

    async fn try_get_release(&self) -> Result<Option<ClientRelease>, Error> {
        Runner::try_get_release(self).await
    }

    fn run_binary(&mut self) -> Result<(), Error> {
        let child = Runner::run_binary(self, Stdio::inherit())?;
        self.child_proc = Some(child);
        Ok(())
    }

    async fn download_binary(&mut self, release: ClientRelease) -> Result<(), Error> {
        let _downloaded_release = Runner::download_binary(self, release).await?;
        Ok(())
    }

    fn get_bin_path(&self) -> Result<(String, PathBuf), Error> {
        Runner::get_bin_path(self)
    }

    fn delete_downloaded_release(&mut self) -> Result<(), Error> {
        Runner::delete_downloaded_release(self)?;
        Ok(())
    }

    fn terminate_proc_and_wait(&mut self) -> Result<(), Error> {
        Runner::terminate_proc_and_wait(self)?;
        Ok(())
    }

    // Declaring as a static method would highly complicate mocking
    async fn get_request_bytes(&self, url: String) -> Result<Bytes, Error> {
        log::info!("Fetching executable from {}", url);
        let response = reqwest::get(url.clone()).await?;
        Ok(response.bytes().await?)
    }

    fn auto_update(&mut self) -> BoxFuture<'_, Result<(), Error>> {
        Runner::auto_update(self).into_future().boxed()
    }

    fn check_child_proc_alive(&mut self) -> Result<bool, Error> {
        Runner::check_child_proc_alive(self)
    }

    fn maybe_restart_client(&mut self) -> Result<(), Error> {
        Runner::maybe_restart_client(self)
    }
}

#[async_trait]
pub trait StorageReader {
    async fn read_chain_storage<T: 'static + Decode + Debug>(
        &self,
        subxt_api: &OnlineClient<PolkadotConfig>,
    ) -> Result<Option<T>, Error>;
}

#[async_trait]
impl StorageReader for Runner {
    async fn read_chain_storage<T: 'static + Decode + Debug>(
        &self,
        subxt_api: &OnlineClient<PolkadotConfig>,
    ) -> Result<Option<T>, Error> {
        Runner::read_chain_storage(subxt_api).await
    }
}

pub async fn subxt_api(url: &str) -> Result<OnlineClient<PolkadotConfig>, Error> {
    Ok(OnlineClient::from_url(url).await?)
}

pub fn custom_retry_config() -> ExponentialBackoff {
    ExponentialBackoff {
        initial_interval: RETRY_INTERVAL,
        max_elapsed_time: Some(RETRY_TIMEOUT),
        multiplier: RETRY_MULTIPLIER,
        ..ExponentialBackoff::default()
    }
}

pub fn retry_with_log<T, F>(mut f: F, log_msg: String) -> Result<T, Error>
where
    F: FnMut() -> Result<T, Error>,
{
    retry(custom_retry_config(), || {
        f().map_err(|e| {
            log::info!("{}: {}. Retrying...", log_msg, e.to_string());
            BackoffError::Transient(e)
        })
    })
    .map_err(Into::into)
}

pub async fn retry_with_log_async<'a, T, F, E>(f: F, log_msg: String) -> Result<T, Error>
where
    F: Fn() -> BoxFuture<'a, Result<T, E>>,
    E: Into<Error> + Sized + Display,
{
    backoff::future::retry(custom_retry_config(), || async {
        f().await.map_err(|e| {
            log::info!("{}: {}. Retrying...", log_msg, e.to_string());
            BackoffError::Transient(e)
        })
    })
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use bytes::Bytes;
    use codec::Decode;

    use futures::future::BoxFuture;
    use sp_core::H256;
    use tempdir::TempDir;

    use std::{
        convert::TryInto,
        fmt::Debug,
        fs::{self, File},
        io::Write,
        os::unix::prelude::PermissionsExt,
        path::PathBuf,
        process::{Child, Command, Stdio},
        str::FromStr,
        thread,
    };

    use signal_hook::consts::*;
    use signal_hook_tokio::Signals;

    use crate::error::Error;

    use super::*;

    use sysinfo::{Pid, System, SystemExt};

    mockall::mock! {
        Runner {}

        #[async_trait]
        pub trait RunnerExt {
            fn subxt_api(&self) -> &OnlineClient<PolkadotConfig>;
            fn vault_args(&self) -> &Vec<String>;
            fn child_proc(&mut self) -> &mut Option<Child>;
            fn set_child_proc(&mut self, child_proc: Option<Child>);
            fn downloaded_release(&self) -> &Option<DownloadedRelease>;
            fn set_downloaded_release(&mut self, downloaded_release: Option<DownloadedRelease>);
            fn download_path(&self) -> &PathBuf;
            fn parachain_url(&self) -> String;
            async fn try_get_release(&self) -> Result<Option<ClientRelease>, Error>;
            async fn download_binary(&mut self, release: ClientRelease) -> Result<(), Error>;
            fn get_bin_path(&self) -> Result<(String, PathBuf), Error>;
            fn delete_downloaded_release(&mut self) -> Result<(), Error>;
            fn run_binary(&mut self) -> Result<(), Error>;
            fn terminate_proc_and_wait(&mut self) -> Result<(), Error>;
            async fn get_request_bytes(&self, url: String) -> Result<Bytes, Error>;
            fn auto_update(&mut self) ->  BoxFuture<'static, Result<(), Error>>;
            fn check_child_proc_alive(&mut self) -> Result<bool, Error>;
            fn maybe_restart_client(&mut self) -> Result<(), Error>;
        }

        #[async_trait]
        pub trait StorageReader {
            async fn read_chain_storage<T: 'static + Decode + Debug>(
                &self,
                subxt_api: &OnlineClient<PolkadotConfig>,
            ) -> Result<Option<T>, Error>;
        }
    }

    #[tokio::test]
    async fn test_runner_download_binary() {
        let mut runner = MockRunner::default();
        let tmp = TempDir::new("runner-tests").expect("failed to create tempdir");
        let mock_path = tmp.path().clone().join("vault-standalone-metadata");
        let moved_mock_path = tmp.path().clone().join("vault-standalone-metadata");
        let mock_bin_name = "vault-standalone-metadata".to_string();

        let client_release = ClientRelease {
            uri: "https://github.com/interlay/interbtc-clients/releases/download/1.15.0/vault-standalone-metadata"
                .to_string(),
            code_hash: H256::default(),
        };

        runner
            .expect_get_bin_path()
            .returning(move || Ok(("vault-standalone-metadata".to_string(), moved_mock_path.clone())));
        runner
            .expect_get_request_bytes()
            .returning(|_| Ok(Bytes::from_static(&[1, 2, 3, 4])));
        runner.expect_set_downloaded_release().return_const(());

        let downloaded_release = Runner::download_binary(&mut runner, client_release.clone())
            .await
            .unwrap();
        assert_eq!(
            downloaded_release,
            DownloadedRelease {
                release: client_release,
                path: mock_path.clone(),
                bin_name: mock_bin_name
            }
        );

        let meta = std::fs::metadata(mock_path.clone()).unwrap();
        // The POSIX mode returned by `Permissions::mode()` contains two kinds of
        // information: the file type code, and the access permission bits.
        // Since the executable is a regular file, its file type code fits the
        // `S_IFREG` bit mask (`0o0100000`).
        // Sources:
        // - https://www.gnu.org/software/libc/manual/html_node/Testing-File-Type.html
        // - https://en.wikibooks.org/wiki/C_Programming/POSIX_Reference/sys/stat.h
        assert_eq!(
            meta.permissions(),
            // Expect the mode to include both the file type (`0100000`) and file permissions (`700`).
            fs::Permissions::from_mode(0o0100700)
        );

        let file_content = fs::read(mock_path.clone()).unwrap();
        assert_eq!(file_content, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_runner_get_bin_path() {
        let mut runner = MockRunner::default();
        runner
            .expect_download_path()
            .return_const(PathBuf::from_str("./mock_download_dir").unwrap());
        let (bin_name, bin_path) = Runner::get_bin_path(&runner).unwrap();
        assert_eq!(bin_name, "vault".to_string());
        assert_eq!(bin_path, PathBuf::from_str("./mock_download_dir/vault").unwrap());
    }

    #[tokio::test]
    async fn test_runner_delete_downloaded_release() {
        let tmp = TempDir::new("runner-tests").expect("failed to create tempdir");
        // Create dummy file
        let mock_path = tmp.path().join("mock_file");
        File::create(mock_path.clone()).unwrap();

        let mut runner = MockRunner::default();
        let downloaded_release = DownloadedRelease {
            release: ClientRelease {
                uri: String::default(),
                code_hash: H256::default(),
            },
            path: mock_path.clone(),
            bin_name: String::default(),
        };
        runner
            .expect_downloaded_release()
            .return_const(Some(downloaded_release));
        runner.expect_set_downloaded_release().return_const(());

        Runner::delete_downloaded_release(&mut runner).unwrap();
        assert_eq!(mock_path.exists(), false);
    }

    #[tokio::test]
    async fn test_runner_terminate_proc_and_wait() {
        // spawn long-running child process
        let mut runner = MockRunner::default();
        runner
            .expect_child_proc()
            .returning(|| Some(Command::new("sleep").arg("100").spawn().unwrap()));
        runner.expect_set_child_proc().return_const(());
        let pid = Runner::terminate_proc_and_wait(&mut runner).unwrap();
        let pid_i32: i32 = pid.try_into().unwrap();
        let s = System::new_all();
        // Get all running processes
        let processes = s.processes();
        // Get the child process based on its pid
        let child_process = processes.get(&Pid::from(pid_i32));

        assert_eq!(child_process.is_none(), true);
    }

    #[tokio::test]
    async fn test_runner_run_binary_with_retry() {
        let tmp = TempDir::new("runner-tests").expect("failed to create tempdir");

        let mock_executable_path = tmp.path().join("print_cli_input");
        {
            let mut file = OpenOptions::new()
                .read(true)
                .write(true)
                .mode(0o700)
                .create(true)
                .open(mock_executable_path.clone())
                .unwrap();

            // Script that prints CLI input to stdout
            file.write_all(b"#!/bin/bash\necho $@").unwrap();

            file.sync_all().unwrap();
            // drop `file` here to close it and avoid `ExecutableFileBusy` errors
        }

        let mut runner = MockRunner::default();
        let mock_vault_args: Vec<String> = vec![
            "--bitcoin-rpc-url",
            "http://localhost:18443",
            "--bitcoin-rpc-user",
            "rpcuser",
            "--bitcoin-rpc-pass",
            "rpcpassword",
            "--keyfile",
            "keyfile.json",
            "--keyname",
            "0xa81f76187f1e5d2059f67439c4242a92a5cd66a409579db73f156c6e2aae5102",
            "--faucet-url",
            "http://localhost:3033",
            "--auto-register=KSM=faucet",
            "--btc-parachain-url",
            "ws://localhost:9944",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        let mock_downloaded_release = DownloadedRelease {
            release: ClientRelease::default(),
            path: mock_executable_path.clone(),
            bin_name: String::default(),
        };
        runner.expect_child_proc().return_var(None);
        runner
            .expect_downloaded_release()
            .return_const(Some(mock_downloaded_release));
        runner.expect_vault_args().return_const(mock_vault_args.clone());
        runner.expect_set_child_proc().return_const(());
        let child = Runner::run_binary(&mut runner, Stdio::piped()).unwrap();

        let output = child.wait_with_output().unwrap();

        let mut expected_output = mock_vault_args.join(" ");
        expected_output.push('\n');
        assert_eq!(output.stdout, expected_output.as_bytes());
    }

    #[tokio::test]
    async fn test_runner_terminate_child_proc_on_signal() {
        let mut runner = MockRunner::default();
        runner.expect_terminate_proc_and_wait().once().returning(|| Ok(()));
        runner.expect_auto_update().returning(|| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(100_000)).await;
                Ok(())
            })
        });
        let shutdown_signals = Signals::new(&[SIGHUP, SIGTERM, SIGINT, SIGQUIT]).unwrap();
        let task = tokio::spawn(Runner::run(Box::new(runner), shutdown_signals));
        // Wait for the signals iterator to be polled
        // This `sleep` is based on the test case in `signal-hook-tokio` itself:
        // https://github.com/vorner/signal-hook/blob/a9e5ca5e46c9c8e6de89ff1b3ce63c5ff89cd708/signal-hook-tokio/tests/tests.rs#L50
        thread::sleep(Duration::from_millis(100));
        signal_hook::low_level::raise(SIGTERM).unwrap();
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_runner_terminate_child_proc_on_crash() {
        let mut runner = MockRunner::default();
        // Assume the auto-updater crashes
        runner.expect_auto_update().returning(|| {
            // return an arbitrary error
            Box::pin(async { Err(Error::ProcessTerminationFailure) })
        });
        // The child process must be killed before shutting down the runner
        runner.expect_terminate_proc_and_wait().once().returning(|| Ok(()));

        let shutdown_signals = Signals::new(&[]).unwrap();
        let task = tokio::spawn(Runner::run(Box::new(runner), shutdown_signals));
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_runner_child_restarts_if_crashed() {
        let mut runner = MockRunner::default();
        runner.expect_check_child_proc_alive().returning(|| Ok(false));

        // The test passes as long as `run_binary` is called
        runner.expect_run_binary().once().returning(|| Ok(()));
        Runner::maybe_restart_client(&mut runner).unwrap();
    }

    #[tokio::test]
    async fn test_runner_terminate_child_process_does_not_throw() {
        let mut runner = MockRunner::default();
        runner.expect_child_proc().return_var(None);
        assert_eq!(Runner::terminate_proc_and_wait(&mut runner).unwrap(), 0);
    }
}
