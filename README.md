# Treemux

[![Documentation](https://img.shields.io/badge/docs-0.5.1-4d76ae?style=for-the-badge)](https://docs.rs/treemux/0.5.1)
[![Version](https://img.shields.io/crates/v/treemux?style=for-the-badge)](https://crates.io/crates/treemux)
[![License](https://img.shields.io/crates/l/treemux?style=for-the-badge)](https://crates.io/crates/treemux)
[![Actions](https://img.shields.io/github/workflow/status/casualjim/rs-treemux/Rust/master?style=for-the-badge)](https://github.com/casualjim/rs-treemux/actions)

Treemux is a lightweight high performance HTTP request router.

This router supports variables in the routing pattern and matches against the request method. It also scales very well.

The router is optimized for high performance and a small memory footprint. It scales well even with very long paths and a large number of routes. A compressing dynamic trie (radix tree) structure is used for efficient matching.

Treemux started as a fork of [httprouter-rs](https://github.com/ibraheemdev/httprouter-rs) by @ibraheemdev, but is mostly rewritten by now.
If you're familiar with the go world, this is the equivalent of [httptreemux](https://github.com/dimfeld/httptreemux) from @dimfeld vs [httprouter](https://github.com/julienschmidt/httprouter) from @julienschmidt. It also adds some middleware support, based on `tower-layer`.

## Usage

Here is a simple example:

```rust ,no_run
use treemux::{middleware_fn, Treemux, RouterBuilder, Params, RequestExt};
use treemux::middlewares;
use std::convert::Infallible;
use hyper::{Request, Response, Body};
use hyper::http::Error;

async fn index(_: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::new("Hello, World!".into()))
}

async fn hello(req: Request<Body>) -> Result<Response<Body>, Error> {
  let user_name = req.params().get("user").unwrap();
  Ok(Response::new(format!("Hello, {}", user_name).into()))
}

#[tokio::main]
async fn main() {
  let router = Treemux::builder();
  let mut router = router.middleware(middleware_fn(middlewares::log_requests));
  router.get("/", index);
  router.get("/hello/:user", hello);

  hyper::Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await;
}
```

### Handler

The handler is a `RequestHandler` and can be created from a simple async function `async fn(hyper::Request) -> Result<hyper::Response<Body>, http::Error>`.

### Middleware

The router supports middleware stacks. Middlewares are implementations of [tower layers](https://docs.rs/tower-layer/latest/tower_layer/trait.Layer.html) `Layer<RequestHandler, Service = RequestHandler>`.  There is a helper function to convert closures into layers. See the [example](examples/middlewares.rs).

### Routing scopes

You can scope routes to a base path to keep it a bit more DRY, this supports named parameters too.

```rust ,no_run
use hyper::{Body, Request, Response, Server};

use treemux::{RequestExt, RouterBuilder, Treemux};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
  femme::with_level(femme::LevelFilter::Debug);

  let mut router = Treemux::builder();
  router.get("/docs", |_req| async {
    Ok(Response::new("This displays some docs".into()))
  });

  let mut api_group = router.scope("/api");
  api_group.get("/todos/:id", |req: Request<Body>| async move {
    let body = format!(
      "{} {} id={}",
      req.method(),
      req.route(),
      req.params().get("id").unwrap().to_string()
    );
    Ok(Response::new(body.into()))
  });

  let _server = Server::bind(&([127, 0, 0, 1], 3000).into())
    .serve(router.into_service())
    .await?;
  Ok(())
}
```

### Routing Rules

Each variable in a path may match on 1 segment only, except for an optional catch-all variable at the end of the URL.

Some examples of valid URL patterns are:

* /post/all
* /post/:postid
* /post/:postid/page/:page
* /post/:postid/:page
* /images/*path
* /favicon.ico
* /:year/:month/
* /:year/:month/:post
* /:page

Note that all of the above URL patterns may exist concurrently in the router.

Path elements starting with `:` indicate a wildcard in the path. A wildcard will only match on a single path segment. That is, the pattern /post/:postid will match on /post/1 or /post/1/, but not /post/1/2.

A path element starting with `*` is a catch-all, whose value will be a string containing all text in the URL matched by the wildcards. For example, with a pattern of /images/*path and a requested URL images/abc/def, path would contain abc/def. A catch-all path will not match an empty string, so in this example a separate route would need to be installed if you also want to match /images/.

#### Using : and * in routing patterns

The characters `:` and `*` can be used at the beginning of a path segment by escaping them with a backslash. A double backslash at the beginning of a segment is interpreted as a single backslash. These escapes are only checked at the very beginning of a path segment; they are not necessary or processed elsewhere in a token.

```ignore
router.get("/foo/\\*starToken", handler) // matches /foo/*starToken
router.get("/foo/star*inTheMiddle", handler) // matches /foo/star*inTheMiddle
router.get("/foo/starBackslash\\*", handler) // matches /foo/starBackslash\*
router.get("/foo/\\\\*backslashWithStar", handler) // matches /foo/\*backslashWithStar
```

### Routing Groups

Lets you create a new group of routes with a given path prefix.  Makes it easier to create clusters of paths like:

* `/api/v1/foo`
* `/api/v1/bar`

To use this you do:

```rust ,ignore
let mut router = Treemux::builder();
let mut api = router.scope("/api/v1");
api.get("/foo", fooHandler) // becomes /api/v1/foo
api.get("/bar", barHandler) // becomes /api/v1/bar
```

### Routing Priority

The priority rules in the router are simple.

1. Static path segments take the highest priority. If a segment and its subtree are able to match the URL, that match is returned.
1. Wildcards take second priority. For a particular wildcard to match, that wildcard and its subtree must match the URL.
1. Finally, a catch-all rule will match when the earlier path segments have matched, and none of the static or wildcard conditions have matched. Catch-all rules must be at the end of a pattern.

So with the following patterns adapted from [simpleblog](https://www.github.com/dimfeld/simpleblog), we'll see certain matches:

```rust ,ignore
let mut router = Treemux::builder();
router.get("/:page", pageHandler)
router.get("/:year/:month/:post", postHandler)
router.get("/:year/:month", archiveHandler)
router.get("/images/*path", staticHandler)
router.get("/favicon.ico", staticHandler)
```

#### Example scenarios

* `/abc` will match `/:page`
* `/2014/05` will match `/:year/:month`
* `/2014/05/really-great-blog-post` will match `/:year/:month/:post`
* `/images/CoolImage.gif` will match `/images/*path`
* `/images/2014/05/MayImage.jpg` will also match `/images/*path`, with all the text after `/images` stored in the variable path.
* `/favicon.ico` will match `/favicon.ico`

### Special Method Behavior

If [Treemux.head_can_use_get](https://docs.rs/treemux/latest/treemux/struct.Builder.html#structfield.head_can_use_get) is set to true, the router will call the `GET` handler for a pattern when a `HEAD` request is processed, if no `HEAD` handler has been added for that pattern. This behavior is enabled by default.

Hyper already already handles the `HEAD` method correctly by sending only the header, so in most cases your handlers will not need any special cases for it.

By default [Treemux::global_options](https://docs.rs/treemux/latest/treemux/struct.Builder.html#method.global_options) is a null handler that doesn't affect your routing. If you set the handler, it will be called on `OPTIONS` requests to a path already registered by another method. If you set a path specific handler by using [Treeemux::options](https://docs.rs/treemux/latest/treemux/trait.RouterBuilder.html#method.options), it will override the global options handler for that path.

```rust
use treemux::{Treemux, RouterBuilder};
use hyper::{Request, Response, Body};
use hyper::http::Error;

async fn global_options(_: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::builder()
      .header("Access-Control-Allow-Methods", "Allow")
      .header("Access-Control-Allow-Origin", "*")
      .body(Body::empty())
      .unwrap())
}

async fn options_override(_: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::builder()
      .header("Access-Control-Allow-Methods", "Allow")
      .header("Access-Control-Allow-Origin", "ui.example.com")
      .body(Body::empty())
      .unwrap())
}


fn main() {
  let mut router = Treemux::builder();
  router.global_options(global_options);
  router.options("/otheropts", options_override);
}
```

### Trailing Slashes

The router has special handling for paths with trailing slashes. If a pattern is added to the router with a trailing slash, any matches on that pattern without a trailing slash will be redirected to the version with the slash. If a pattern does not have a trailing slash, matches on that pattern with a trailing slash will be redirected to the version without.

The trailing slash flag is only stored once for a pattern. That is, if a pattern is added for a method with a trailing slash, all other methods for that pattern will also be considered to have a trailing slash, regardless of whether or not it is specified for those methods too.
However this behavior can be turned off by setting TreeMux.RedirectTrailingSlash to false. By default it is set to true.

One exception to this rule is catch-all patterns. By default, trailing slash redirection is disabled on catch-all patterns, since the structure of the entire URL and the desired patterns can not be predicted. If trailing slash removal is desired on catch-all patterns, set TreeMux.RemoveCatchAllTrailingSlash to true.

```rust ,ignore
let mut router = Treemux::builder()
router.get("/about", pageHandler)
router.get("/posts/", postIndexHandler)
router.post("/posts", postFormHandler)

/*
GET /about will match normally.
GET /about/ will redirect to /about.
GET /posts will redirect to /posts/.
GET /posts/ will match normally.
POST /posts will redirect to /posts/, because the GET method used a trailing slash.
*/
```

### Custom Redirects

RedirectBehavior sets the behavior when the router redirects the request to the canonical version of the requested URL using RedirectTrailingSlash or RedirectClean. The default behavior is to return a 301 status, redirecting the browser to the version of the URL that matches the given pattern.

These are the values accepted for RedirectBehavior. You may also add these values to the RedirectMethodBehavior map to define custom per-method redirect behavior.

* Redirect301 - HTTP 301 Moved Permanently; this is the default.
* Redirect307 - HTTP/1.1 Temporary Redirect
* Redirect308 - RFC7538 Permanent Redirect
* UseHandler - Don't redirect to the canonical path. Just call the handler instead.

#### Rationale/Usage

On a `POST` request, most browsers that receive a 301 will submit a `GET` request to the redirected URL, meaning that any data will likely be lost. If you want to handle and avoid this behavior, you may use `Redirect307`, which causes most browsers to resubmit the request using the original method and request body.

Since `307` is supposed to be a temporary redirect, the new `308` status code has been proposed, which is treated the same, except it indicates correctly that the redirection is permanent. The big caveat here is that the RFC is relatively recent, and older or non-compliant browsers will not handle it. Therefore its use is not recommended unless you really know what you're doing.

Finally, the `UseHandler` value will simply call the handler function for the pattern, without redirecting to the canonical version of the URL.

#### Escaped Slashes

Hyper automatically processes escaped characters in a URL, converting + to a space and %XX to the corresponding character. This can present issues when the URL contains a %2f, which is unescaped to '/'. This isn't an issue for most applications, but it will prevent the router from correctly matching paths and wildcards.

For example, the pattern `/post/:post` would not match on `/post/abc%2fdef`, which is unescaped to `/post/abc/def`. The desired behavior is that it matches, and the `post` wildcard is set to `abc/def`.

Therefore, this router defaults to using the raw URL, stored in the Request.RequestURI variable. Matching wildcards and catch-alls are then unescaped, to give the desired behavior.

## Error Handlers

### Not found handler

You can use another handler, to handle requests which could not be matched by this router by using the [`Router::not_found`](https://docs.rs/treemux/latest/treemux/struct.Builder.html#method.not_found) handler.

The `not_found` handler can for example be used to return a 404 page:

```rust
use treemux::Treemux;
use hyper::{Request, Response, Body};
use hyper::http::Error;

async fn not_found(_req: Request<Body>) -> Result<Response<Body>, Error> {
  Ok(Response::builder()
    .status(404)
    .body(Body::empty())
    .unwrap())
}

fn main() {
  let mut router = Treemux::builder();
  router.not_found(not_found);
}
```

### Method not allowed handler

If a pattern matches, but the pattern does not have an associated handler for the requested method, the router calls the [`Treemux::method_not_allowed`](https://docs.rs/treemux/latest/treemux/struct.Builder.html#method.method_not_allowed) handler. The default
version of this handler just writes the status code `405` and sets the `Allow` response header field appropriately.

```rust ,no_run
use treemux::{AllowedMethods, RouterBuilder, Treemux};
use hyper::{
  header::{ALLOW, CONTENT_TYPE},
  http, Request, Response, StatusCode, Body
};

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
      .body(format!(
        r#"{{"message":"Method not allowed, try {}"}}"#,
        allowed
      ).into())
      .unwrap(),
  )
}

fn main() {
  let mut router = Treemux::builder();
  router.method_not_allowed(method_not_allowed);
}
```

### Panic Handling

[Treemux::panic_handler](https://docs.rs/treemux/latest/treemux/struct.Builder.html#method.handle_panics) can be set to provide custom panic handling. The default panic handler logs the stack trace at error level.

## Extra use cases

### Multi-domain / Sub-domains

Here is a quick example: Does your server serve multiple domains / hosts? You want to use sub-domains? Define a router per host!

```rust ,no_run
use treemux::{Treemux, RouterBuilder};
use hyper::service::{make_service_fn, service_fn};
use hyper::{http, Body, Request, Response, Server, StatusCode};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

pub struct HostSwitch(HashMap<String, Treemux>);

impl HostSwitch {
  async fn serve(&self, req: Request<Body>) -> Result<Response<Body>, http::Error> {
    let forbidden = Response::builder()
      .status(StatusCode::FORBIDDEN)
      .body(Body::empty())
      .unwrap();
    match req.headers().get("host") {
      Some(host) => match self.0.get(host.to_str().unwrap()) {
        Some(router) => router.serve(req).await,
        None => Ok(forbidden),
      },
      None => Ok(forbidden),
    }
  }
}

async fn hello(_: Request<Body>) -> Result<Response<Body>, http::Error> {
    Ok(Response::new(Body::default()))
}

#[tokio::main]
async fn main() {
  let mut router = Treemux::builder();
  router.get("/", hello);

  let mut host_switch: HostSwitch = HostSwitch(HashMap::new());
  host_switch.0.insert("example.com:12345".into(), router.into());

  let host_switch = Arc::new(host_switch);
  
  let make_svc = make_service_fn(move |_| {
    let host_switch = host_switch.clone();
    async move {
      Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
        let host_switch = host_switch.clone();
        async move { host_switch.serve(req).await }
      }))
    }
  });

  let server = Server::bind(&([127, 0, 0, 1], 3000).into())
      .serve(make_svc)
      .await;
}
```

### Static files

You can use the router to serve pages from a static file directory with the [static_files helper method](examples/staticfile.rs).

To enable this add the `hyper-staticfile` crate to your cargo.toml.

```rust ,ignore
use hyper::Server;
use treemux::{static_files, RouterBuilder, Treemux};

// requires `hyper-staticfile = "0.6"` in cargo.toml

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
```
