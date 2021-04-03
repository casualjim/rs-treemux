use std::time::SystemTime;

use futures::Future;
use hyper::{header::HeaderValue, http, Body, Request, Response};
use tracing::{error, info, trace, warn};

use crate::Handler;

/// Logs requests with the kvlog macro.
pub fn log_requests<H, R>(next: H) -> Handler
where
  H: Fn(Request<Body>) -> R + Send + Sync + 'static,
  R: Future<Output = Result<Response<Body>, http::Error>> + Send + 'static,
{
  Box::new(move |req: Request<Body>| {
    let start = SystemTime::now();
    let method = req.method().as_str().to_string();
    let path = req.uri().path_and_query().unwrap().as_str().to_string();

    trace!("begin processing request");
    let fut = next(req);
    Box::pin(async move {
      let result = fut.await;
      let took = SystemTime::now().duration_since(start).unwrap();
      match result {
        Ok(mut resp) => {
          if resp.status().is_success() {
            info!(
              message = "Processed request",
              method = method.as_str(),
              path = path.as_str(),
              took = format!("{:?}", took).as_str(),
              status_code = resp.status().as_u16(),
              headers = format!("{:?}", resp.headers()).as_str(),
              body = format!("{:?}", resp.body()).as_str(),
            );
          } else {
            warn!(
              message = "Processed request",
              method = method.as_str(),
              path = path.as_str(),
              took = format!("{:?}", took).as_str(),
              status_code = resp.status().as_u16(),
              headers = format!("{:?}", resp.headers()).as_str(),
              body = format!("{:?}", resp.body()).as_str(),
            );
          }
          let tstr = format!("{:?}", took);
          resp
            .headers_mut()
            .insert("x-request-duration", HeaderValue::from_str(&tstr).unwrap());
          Ok(resp)
        }
        Err(e) => {
          error!("{}", e);
          Err(e)
        }
      }
    })
  })
}
