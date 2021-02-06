use anyhow::Result;
use femme::LevelFilter;
use futures::FutureExt;
use hyper::{
  header::{ALLOW, CONTENT_TYPE},
  http, Response, StatusCode,
};
use hyper::{Body, Request, Server};
use log::info;
use treemux::{middleware_fn, middlewares, AllowedMethods, RouterBuilder, Treemux};

async fn todos(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
  Response::builder()
    .status(StatusCode::OK)
    .header(CONTENT_TYPE, "application/json; charset=utf-8")
    .body(Body::from(
      r#"[{"content":"the first task"}, {"content":"the first done task","done":true}]"#,
    ))
}

async fn not_found(_req: Request<Body>) -> Result<Response<Body>, http::Error> {
  Ok(
    Response::builder()
      .status(StatusCode::NOT_FOUND)
      .header(CONTENT_TYPE, "application/json; charset=utf-8")
      .body(Body::from(r#"{"message":"Not found"}"#))
      .unwrap(),
  )
}

async fn method_not_allowed(req: Request<Body>) -> Result<Response<Body>, http::Error> {
  let allowed = req
    .extensions()
    .get::<AllowedMethods>()
    .map(|v| {
      v.methods()
        .iter()
        .map(|v| v.as_str().to_string())
        .collect::<Vec<String>>()
        .join(", ")
    })
    .unwrap_or_default();

  Ok(
    Response::builder()
      .status(StatusCode::METHOD_NOT_ALLOWED)
      .header(CONTENT_TYPE, "application/json; charset=utf-8")
      .header(ALLOW, &allowed)
      .body(Body::from(format!(
        r#"{{"message":"Method not allowed, try {}"}}"#,
        allowed
      )))
      .unwrap(),
  )
}

#[tokio::main]
async fn main() -> Result<()> {
  femme::with_level(LevelFilter::Debug);
  let router = Treemux::builder();

  let router = router.middleware(middleware_fn(move |next| {
    info!("middleware constructor");
    move |request| {
      info!("before handling request");
      next(request).map(|response| {
        info!("after processing the request");
        response
      })
    }
  }));
  let mut router = router.middleware(middleware_fn(middlewares::log_requests));

  router.not_found(not_found);
  router.method_not_allowed(method_not_allowed);

  router.get("/about", |req| async move {
    info!("{:?}", req);
    Response::builder()
      .status(StatusCode::OK)
      .header(CONTENT_TYPE, "application/json; charset=utf-8")
      .body(Body::from(
        r#"[{"content":"the first task"}, {"content":"the first done task","done":true}]"#,
      ))
  });
  router.get("/todos", todos);

  Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await?;
  Ok(())
}
