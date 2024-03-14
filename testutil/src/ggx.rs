use testcontainers::{
    core::{Image, WaitFor},
    Container, ImageArgs,
};

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct GgxNodeImage {
    // these image:tag will be used
    image: String,
    tag: String,
}

impl Default for GgxNodeImage {
    fn default() -> Self {
        Self {
            // default image+tag
            image: "public.ecr.aws/k7w7q6c4/ggxchain-node".to_string(),

            // update this tag to the latest when necessary
            tag: "brooklyn-9cf57e91".to_string(),
        }
    }
}

impl GgxNodeImage {
    pub fn with_image(mut self, image: String) -> Self {
        self.image = image;
        self
    }

    pub fn with_tag(mut self, tag: String) -> Self {
        self.tag = tag;
        self
    }
}

impl Image for GgxNodeImage {
    type Args = GgxNodeArgs;

    fn name(&self) -> String {
        self.image.clone()
    }

    fn tag(&self) -> String {
        self.tag.clone()
    }

    fn expose_ports(&self) -> Vec<u16> {
        vec![
            9944,  // rpc
            30333, // p2p
        ]
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stderr("Running JSON-RPC server: addr=")]
    }
}

#[derive(Debug, Clone)]
pub struct GgxNodeArgs {
    args: Vec<String>,
}
impl Default for GgxNodeArgs {
    fn default() -> Self {
        Self {
            args: vec![
                "--rpc-external",
                "--rpc-methods=unsafe",
                "--unsafe-rpc-external",
                "--dev",
                "--rpc-port=9944",
                // disable unused features
                "--no-prometheus",
                "--no-telemetry",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl ImageArgs for GgxNodeArgs {
    fn into_iterator(self) -> Box<dyn Iterator<Item = String>> {
        Box::new(self.args.into_iter())
    }
}

pub struct GgxNodeContainer<'d>(pub Container<'d, GgxNodeImage>);
impl<'d> GgxNodeContainer<'d> {
    pub fn get_rpc_port(&self) -> u16 {
        self.0.get_host_port_ipv4(9944)
    }

    pub fn get_host(&self) -> String {
        "127.0.0.1".to_string()
    }

    // TODO(Bohdan): add fn api() that will return a client
}

#[cfg(test)]
mod tests {
    use super::{GgxNodeContainer, GgxNodeImage};
    use testcontainers::{clients::Cli, RunnableImage};

    #[tokio::test]
    async fn test_ggx_node() {
        env_logger::init();
        let docker = Cli::default();
        let image: RunnableImage<GgxNodeImage> = GgxNodeImage::default().into();
        let node = GgxNodeContainer(docker.run(image));

        let host = node.get_host();
        let port = node.get_rpc_port();
        println!("Node is running at {}:{}", host, port);
        assert_ne!(port, 9944); // port will be random
    }
}
