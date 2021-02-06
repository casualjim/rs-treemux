use std::{convert::Infallible, sync::Arc};

use hyper::{
  service::{make_service_fn, service_fn},
  Body, Request, Server,
};

use treemux::Treemux;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  let router = Treemux::builder();

  let router: Arc<Treemux> = Arc::new(router.into());

  let make_svc = make_service_fn(move |_| {
    let router = router.clone();
    async move {
      Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
        let router = router.clone();
        async move {
          let svc = router.clone();
          svc.serve(req).await
        }
      }))
    }
  });

  let _server = Server::bind(&([127, 0, 0, 1], 3000).into()).serve(make_svc).await?;
  Ok(())
}
