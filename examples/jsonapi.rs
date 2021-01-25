use anyhow::Result;
use femme::LevelFilter;
use hyper::{
  header::{ALLOW, CONTENT_TYPE},
  http, Method, Response, StatusCode,
};
use hyper::{Body, Request, Server};
use log::info;
use treemux::{middlewares, Handler, RouterBuilder, Treemux};

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

async fn method_not_allowed(_req: Request<Body>, allowed: Vec<Method>) -> Result<Response<Body>, http::Error> {
  let allowed_methods = allowed
    .iter()
    .map(|v| v.as_str().to_string())
    .collect::<Vec<String>>()
    .join(", ");
  Ok(
    Response::builder()
      .status(StatusCode::METHOD_NOT_ALLOWED)
      .header(CONTENT_TYPE, "application/json; charset=utf-8")
      .header(ALLOW, &allowed_methods)
      .body(Body::from(format!(
        r#"{{"message":"Method not allowed, try {}"}}"#,
        allowed_methods
      )))
      .unwrap(),
  )
}

#[tokio::main]
async fn main() -> Result<()> {
  femme::with_level(LevelFilter::Debug);
  let mut router = Treemux::builder();

  router.middleware(middlewares::log_requests);

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
    .await
    .unwrap();
  Ok(())
}
