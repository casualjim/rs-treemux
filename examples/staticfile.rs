use hyper::Server;
use treemux::{static_files, RouterBuilder, Treemux};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let mut router = Treemux::builder();
  router.get("/", static_files("./examples/static"));
  router.get("/*", static_files("./examples/static"));

  Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await?;
  Ok(())
}
