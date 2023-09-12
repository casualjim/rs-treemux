use futures::FutureExt;
use hyper::{Response, Server};
use treemux::{middleware_fn, RouterBuilder, Treemux};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  tracing_subscriber::fmt::init();

  let mut router = Treemux::builder().middleware(middleware_fn(move |next| {
    move |request| {
      next(request).map(|result| {
        result.map(|mut response| {
          response
            .headers_mut()
            .insert("Access-Control-Allow-Origin", "*".parse().unwrap());
          response
        })
      })
    }
  }));
  router.get("/", |_req| async { Ok(Response::new("Hello, World!".into())) });

  Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await?;
  Ok(())
}
