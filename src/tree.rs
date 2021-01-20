use hyper::Method;
use percent_encoding::percent_decode_str;
use std::{borrow::Cow, collections::HashMap, fmt::Debug, iter::FromIterator, ops::Index, vec};

/// The response returned when getting the value for a specific path with
/// [`Node::match_path`](crate::Node::match_path)
#[derive(Debug)]
pub struct Match<'a, V> {
  /// The value stored under the matched node.
  pub value: Option<&'a V>,
  /// The route parameters. See [parameters](/index.html#parameters) for more details.
  params: Vec<Cow<'a, str>>,
  /// The route path
  pub path: Cow<'a, str>,
  pub pattern: Cow<'a, str>,
  pub implicit_head: bool,
  pub add_slash: bool,
  /// The route parameters. See [parameters](/index.html#parameters) for more details.
  param_names: Vec<Cow<'a, str>>,
  // pub parameters: HashMap<Cow<'a, str>, Cow<'a, str>>,
}

impl<'a, V> Match<'a, V> {
  /// The route parameters. See [parameters](/index.html#parameters) for more details.
  pub fn parameters(&self) -> Params {
    self.param_names.clone().into_iter().zip(self.params.clone()).collect()
  }
}

/// Param is a single URL parameter, consisting of a key and a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Param<'a> {
  pub key: Cow<'a, str>,
  pub value: Cow<'a, str>,
}

impl<'a> Param<'a> {
  pub fn new(key: Cow<'a, str>, value: Cow<'a, str>) -> Self {
    Self { key, value }
  }
}

impl<'a> From<(&'a str, &'a str)> for Param<'a> {
  fn from(input: (&'a str, &'a str)) -> Self {
    Param::new(input.0.into(), input.1.into())
  }
}

impl<'a, 'b> From<(&'a Cow<'b, str>, &'a Cow<'b, str>)> for Param<'a>
where
  'b: 'a,
{
  fn from(input: (&'a Cow<'b, str>, &'a Cow<'b, str>)) -> Self {
    Param::new(input.0.clone(), input.1.clone())
  }
}

impl<'a> From<(Cow<'a, str>, Cow<'a, str>)> for Param<'a> {
  fn from(input: (Cow<'a, str>, Cow<'a, str>)) -> Self {
    Param::new(input.0, input.1)
  }
}

/// A `Vec` of `Param` returned by a route match.
/// There are two ways to retrieve the value of a parameter:
///  1) by the name of the parameter
/// ```rust
///  # use matchit::Params;
///  # let params = Params::default();

///  let user = params.by_name("user"); // defined by :user or *user
/// ```
///  2) by the index of the parameter. This way you can also get the name (key)
/// ```rust,no_run
///  # use matchit::Params;
///  # let params = Params::default();
///  let third_key = &params[2].key;   // the name of the 3rd parameter
///  let third_value = &params[2].value; // the value of the 3rd parameter
/// ```
#[derive(Debug, PartialEq)]
pub struct Params<'a>(pub Vec<Param<'a>>);

impl<'a> Default for Params<'a> {
  fn default() -> Self {
    Self(Vec::new())
  }
}

impl<'a> Index<usize> for Params<'a> {
  type Output = Param<'a>;

  #[inline]
  fn index(&self, i: usize) -> &Param<'a> {
    &self.0[i]
  }
}

impl<'a> std::ops::IndexMut<usize> for Params<'a> {
  fn index_mut(&mut self, i: usize) -> &mut Param<'a> {
    &mut self.0[i]
  }
}

impl<'a> FromIterator<(&'a str, &'a str)> for Params<'a> {
  fn from_iter<T: IntoIterator<Item = (&'a str, &'a str)>>(iter: T) -> Self {
    Params(iter.into_iter().map(Into::into).collect())
  }
}

impl<'a, 'b> FromIterator<(&'a Cow<'b, str>, &'a Cow<'b, str>)> for Params<'a>
where
  'b: 'a,
{
  fn from_iter<T: IntoIterator<Item = (&'a Cow<'b, str>, &'a Cow<'b, str>)>>(iter: T) -> Self {
    Params(iter.into_iter().map(Into::into).collect())
  }
}

impl<'a> FromIterator<(Cow<'a, str>, Cow<'a, str>)> for Params<'a> {
  fn from_iter<T: IntoIterator<Item = (Cow<'a, str>, Cow<'a, str>)>>(iter: T) -> Self {
    Params(iter.into_iter().map(Into::into).collect())
  }
}

impl<'a> Params<'a> {
  /// Returns the value of the first `Param` whose key matches the given name.
  pub fn by_name(&self, name: &str) -> Option<&str> {
    match self.0.iter().find(|param| param.key == name) {
      Some(param) => Some(&param.value),
      None => None,
    }
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  /// Inserts a URL parameter into the vector
  pub fn push<P: Into<Param<'a>>>(&mut self, p: P) {
    self.0.push(p.into());
  }

  pub fn len(&self) -> usize {
    self.0.len()
  }
}

/// A node in radix tree ordered by priority.
///
/// Priority is just the number of values registered in sub nodes
/// (children, grandchildren, and so on..).
#[derive(Debug, PartialEq)]
pub struct Node<'a, V> {
  path: Cow<'a, str>,
  priority: isize,
  static_indices: Vec<char>,
  static_child: Vec<Option<Box<Self>>>,
  wildcard_child: Option<Box<Self>>,
  catch_all_child: Option<Box<Self>>,
  add_slash: bool,
  is_catch_all: bool,
  implicit_head: bool,
  leaf_handler: HashMap<Method, V>,
  leaf_wildcard_names: Option<Vec<Cow<'a, str>>>,
}

#[derive(Debug, PartialEq)]
pub struct Handler<V> {
  pub verb: Method,
  pub value: V,
  pub implicit_head: bool,
  pub add_slash: bool,
}

impl<'a, V> Default for Node<'a, V> {
  fn default() -> Self {
    Self {
      path: Cow::Borrowed(""),
      priority: 0,
      static_indices: vec![],
      static_child: vec![],
      wildcard_child: None,
      catch_all_child: None,
      add_slash: false,
      is_catch_all: false,
      implicit_head: true,
      leaf_handler: HashMap::new(),
      leaf_wildcard_names: None,
    }
  }
}

impl<'a, V> Node<'a, V> {
  pub fn new() -> Self {
    Node {
      path: "/".into(),
      ..Default::default()
    }
  }

  pub fn insert(&mut self, verb: Method, path: Cow<'a, str>, value: V) {
    self.add_path(
      path,
      None,
      false,
      Handler {
        verb,
        value,
        implicit_head: false,
        add_slash: false,
      },
    );
  }

  pub fn search<'b>(&'b self, verb: &Method, path: Cow<'a, str>) -> Option<Match<'b, V>> {
    self.internal_search(verb, path)
  }

  fn internal_search<'b>(&'b self, verb: &Method, path: Cow<'a, str>) -> Option<Match<'b, V>> {
    let path_len = path.len();
    if path.is_empty() {
      if self.leaf_handler.is_empty() {
        return None;
      }

      return Some(Match {
        value: self.leaf_handler.get(&verb),
        params: vec![],
        path,
        pattern: self.path.clone(),
        param_names: self.leaf_wildcard_names.clone().unwrap_or_default(),
        implicit_head: self.implicit_head,
        add_slash: self.add_slash,
      });
    }

    // First see if this matches a static token.
    let first_char = &path.chars().next().unwrap();
    let mut found = None;
    for (i, static_index) in (&self.static_indices).iter().enumerate() {
      if static_index == first_char {
        let child = self.static_child[i].as_ref().unwrap();
        let child_path_len = child.path.len();
        if path_len >= child_path_len && child.path == path[..child_path_len] {
          let next_path = path.chars().skip(child_path_len).collect();
          found = child.search(verb, next_path);
        }
        break;
      }
    }

    // If we found a node and it had a valid handler, then return here. Otherwise
    // let's remember that we found this one, but look for a better match.
    if found.as_ref().filter(|v| v.value.is_some()).is_some() {
      return found;
    }

    if let Some(wildcard_child) = self.wildcard_child.as_ref() {
      let next_slash = path.chars().position(|c| c == '/').unwrap_or(path_len);
      let this_token: Cow<'a, str> = path.chars().take(next_slash).collect();
      let next_token: Cow<'a, str> = path.chars().skip(next_slash).collect();

      if !this_token.is_empty() {
        let wc_match = wildcard_child.search(verb, next_token);

        if wc_match.as_ref().filter(|v| v.value.is_some()).is_some() || (found.is_none() && wc_match.is_some()) {
          let pth = percent_decode_str(this_token.as_ref()).decode_utf8_lossy().to_string();

          if let Some(mut the_match) = wc_match {
            let mut nwparams = vec![pth.into()];
            nwparams.append(&mut the_match.params);
            the_match.params = nwparams;
            if the_match.value.as_ref().is_some() {
              if the_match.param_names.is_empty() {
                the_match.param_names = wildcard_child.leaf_wildcard_names.clone().unwrap_or_default();
              }
              return Some(the_match);
            } else {
              found = Some(the_match);
            }
          } else {
            // Didn't actually find a handler here, so remember that we
            // found a node but also see if we can fall through to the
            // catchall.
            found = wc_match;
          }
        }
      }
    }

    if let Some(catch_all_child) = self.catch_all_child.as_ref() {
      // Hit the catchall, so just assign the whole remaining path if it
      // has a matching handler.
      let handler = catch_all_child.leaf_handler.get(&verb);

      // Found a handler, or we found a catchall node without a handler.
      // Either way, return it since there's nothing left to check after this.
      if handler.is_some() || found.is_none() {
        let pth = percent_decode_str(path.as_ref()).decode_utf8_lossy().to_string();
        return Some(Match {
          value: handler,
          params: vec![pth.clone().into()],
          pattern: pth.into(),
          param_names: catch_all_child.leaf_wildcard_names.clone().unwrap_or_default(),
          path,
          add_slash: catch_all_child.add_slash,
          implicit_head: catch_all_child.implicit_head,
        });
      }
    }

    found
  }

  pub fn dump_tree(&self, prefix: &str, node_type: &str) -> String {
    let mut line = format!(
      "{} {:02} {}{} [{}] {} wildcards {:?}\n",
      prefix,
      self.priority,
      node_type,
      self.path,
      self.static_child.len(),
      self
        .leaf_handler
        .keys()
        .map(|k| k.as_str().to_string())
        .collect::<Vec<String>>()
        .join(","),
      self.leaf_wildcard_names
    );

    let mut pref = prefix.to_string();
    pref.push_str("  ");

    for n in self.static_child.iter().map(|v| v.as_ref()) {
      if let Some(n) = n {
        line.push_str(&n.dump_tree(&pref, ""));
      }
    }

    if let Some(wc) = self.wildcard_child.as_ref() {
      line.push_str(&wc.dump_tree(&pref, ":"))
    }

    if let Some(ca) = self.catch_all_child.as_ref() {
      line.push_str(&ca.dump_tree(&pref, "*"))
    }

    line
  }

  fn sort_static_child(&mut self, i: usize) {
    let mut i = i;
    while i > 0
      && (&self.static_child[i]).as_ref().map(|p| p.priority) > (&self.static_child[i - 1]).as_ref().map(|p| p.priority)
    {
      self.static_child.swap(i - 1, i);
      self.static_indices.swap(i - 1, i);
      i -= 1;
    }
  }

  fn set_handler(&mut self, verb: Method, handler: V, implicit_head: bool, add_slash: bool) {
    if self.leaf_handler.contains_key(&verb) && (verb != Method::HEAD || !self.implicit_head) {
      panic!("{} already handles {}", self.path, verb)
    }

    if verb == Method::HEAD {
      self.implicit_head = implicit_head;
    }
    self.add_slash = add_slash;
    self.leaf_handler.insert(verb, handler);
  }

  fn add_path(
    &mut self,
    path: Cow<'a, str>,
    wildcards: Option<Vec<Cow<'a, str>>>,
    in_static_token: bool,
    handler: Handler<V>,
  ) {
    if path.is_empty() {
      if let Some(ref wildcards) = wildcards {
        // Make sure the current wildcards are the same as the old ones.
        // When they aren't, we have an ambiguous path
        if let Some(leaf_wildcards) = &self.leaf_wildcard_names {
          if wildcards.len() != leaf_wildcards.len() {
            // this should never happen, they said
            panic!("Reached leaf node with differing wildcard slice length. Please report this as a bug.")
          }

          if wildcards != leaf_wildcards {
            panic!("Wildcards {:?} are ambiguous with {:?}.", leaf_wildcards, wildcards);
          }
        } else {
          self.leaf_wildcard_names = Some(wildcards.clone());
        }
      }
      self.set_handler(handler.verb, handler.value, handler.implicit_head, handler.add_slash);
      return;
    }

    let mut c = path.chars().next().unwrap();
    let next_slash = path.chars().position(|c| c == '/');

    let (mut this_token, token_end) = if c == '/' {
      ("/".into(), Some(1))
    } else if next_slash.is_none() {
      let ln = Some(path.len());
      (path.clone().to_string(), ln)
    } else {
      (path.chars().take(next_slash.unwrap_or_default()).collect(), next_slash)
    };

    let remaining_path = path.chars().skip(token_end.unwrap_or_default()).collect();

    if c == '*' && !in_static_token {
      this_token = (&this_token[1..]).to_string();

      if self.catch_all_child.is_none() {
        self.catch_all_child = Some(Box::new(Node {
          is_catch_all: true,
          path: this_token.clone().into(),
          ..Default::default()
        }));
      }

      let cac = self.catch_all_child.as_ref().map(|v| v.path.as_ref());
      if Some(&path[1..]) != cac {
        panic!(
          "Catch-all name in {} doesn't match {}. You probably tried to define overlappig catchalls",
          path,
          cac.unwrap_or_default()
        );
      }

      if next_slash.is_some() {
        panic!("/ after catch-all found in {}", path);
      }

      // let mut to_append = vec![this_token.into()];
      let wildc = wildcards.clone().map_or_else(
        || vec![this_token.clone().into()],
        |mut tokens| {
          tokens.push(this_token.clone().into());
          tokens
        },
      );
      self.catch_all_child.as_mut().map(move |mut n| {
        n.set_handler(handler.verb, handler.value, handler.implicit_head, handler.add_slash);
        n.leaf_wildcard_names = Some(wildc);
        n
      });
    } else if c == ':' && !in_static_token {
      this_token = (&this_token[1..]).into();
      let wil = wildcards
        .map(|mut tokens| {
          tokens.push(this_token.clone().into());
          tokens
        })
        .or_else(|| Some(vec![this_token.clone().into()]));

      if self.wildcard_child.is_none() {
        self.wildcard_child = Some(Box::new(Node {
          path: this_token.into(),
          ..Default::default()
        }));
      }

      // self.wildcard_child;
      self.wildcard_child.as_mut().map(|ch| {
        ch.add_path(remaining_path, wil, false, handler);
        ch
      });
    } else {
      let mut unescaped = false;
      if this_token.len() >= 2
        && !in_static_token
        && (this_token.starts_with('\\')
          && (this_token.chars().nth(1).unwrap() == '*'
            || this_token.chars().nth(1).unwrap() == ':'
            || this_token.chars().nth(1).unwrap() == '\\'))
      {
        c = this_token.chars().nth(1).unwrap();
        // The token starts with a character escaped by a backslash. Drop the backslash.
        this_token = (&this_token[1..]).to_string();
        unescaped = true;
      }

      // Set inStaticToken to ensure that the rest of this token is not mistaken
      // for a wildcard if a prefix split occurs at a '*' or ':'.
      let in_static_token = c != '/';

      let token: Cow<str> = this_token.into();
      // Do we have an existing node that starts with the same letter?
      for (i, indexchar) in self.static_indices.clone().iter().enumerate() {
        if c == *indexchar {
          // Yes. Split it based on the common prefix of the existing
          // node and the new one.
          let mut prefix_split = self.split_common_prefix(i, token.clone());

          self.static_child.get_mut(i).map(|maybe_child| {
            maybe_child.as_mut().map(|child| {
              child.priority += 1;
            })
          });

          if unescaped {
            prefix_split += 1
          }

          self.static_child.get_mut(i).map(|maybe_child| {
            maybe_child.as_mut().map(|child| {
              child.add_path(
                (&path[prefix_split..]).to_string().into(),
                wildcards,
                in_static_token,
                handler,
              )
            })
          });
          self.sort_static_child(i);

          return;
        }
      }

      // No existing node starting with this letter, so create it.
      let mut child = Self {
        path: token,
        ..Default::default()
      };
      // child.set_handler(verb, handler, implicit_head)
      self.static_indices.append(&mut vec![c]);
      child.add_path(remaining_path, wildcards, in_static_token, handler);
      let chld = Some(Box::new(child));
      self.static_child.append(&mut vec![chld]);
    }
  }

  fn split_common_prefix(&mut self, existing_node_index: usize, path: Cow<'a, str>) -> usize {
    let child_node = self.static_child.get(existing_node_index).unwrap().as_ref();

    let contains_path = child_node.filter(|cn| path.starts_with(cn.path.as_ref())).is_some();
    if contains_path {
      // No split needs to be done. Rather, the new path shares the entire
      // prefix with the existing node, so the new node is just a child of
      // the existing one. Or the new path is the same as the existing path,
      // which means that we just move on to the next token.
      let ln = child_node.unwrap().path.len();
      return ln;
    }

    let cn = child_node.unwrap();

    let i = cn
      .path
      .clone()
      .chars()
      .zip(path.chars())
      .take_while(|&(l, r)| l == r)
      .count();
    let common_prefix = path[..i].to_string();
    let child_path: Cow<str> = cn.path.chars().skip(i).collect();
    let vv = Self {
      path: common_prefix.into(),
      priority: cn.priority,
      static_indices: vec![child_path.chars().next().unwrap()],
      static_child: vec![],
      ..Default::default()
    };

    let mut old_child = self.static_child[existing_node_index].replace(Box::new(vv));
    old_child.as_mut().map(|v| {
      v.path = child_path;
      v
    });
    self.static_child.get_mut(existing_node_index).map(|nn| {
      nn.as_mut().map(|nv| {
        nv.static_child.push(old_child);
        nv
      })
    });
    i
  }
}

#[cfg(test)]
mod tests {
  use hyper::{Body, Method, Request, Response, StatusCode};

  use std::{panic, vec};

  use super::{Handler, Node, Params};

  type ReqHandler = Box<dyn Fn(Request<Body>) -> Response<Body>>;

  fn add_path(node: &mut Node<'static, ReqHandler>, path: &'static str) {
    debug!("adding path path={}", path);
    node.insert(
      Method::GET,
      path[1..].into(),
      Box::new(move |mut req: Request<Body>| {
        let params = req.extensions_mut().get_mut::<Params>().unwrap();
        params.push(("path", path));
        // req.extensions_mut().insert(params);
        Response::builder()
          .status(StatusCode::OK)
          .body(Body::from(path))
          .unwrap()
      }),
    );
  }

  #[tokio::test]
  async fn test_tree() {
    std::env::set_var("RUST_LOG", "debug");
    femme::try_with_level(femme::LevelFilter::Trace).ok();

    let mut root = Node::new();
    add_path(&mut root, "/");
    add_path(&mut root, "/i");
    add_path(&mut root, "/i/:aaa");
    add_path(&mut root, "/images");
    add_path(&mut root, "/images/abc.jpg");
    add_path(&mut root, "/images/:imgname");
    add_path(&mut root, "/images/\\*path");
    add_path(&mut root, "/images/\\*patch");
    add_path(&mut root, "/images/*path");
    add_path(&mut root, "/ima");
    add_path(&mut root, "/ima/:par");
    add_path(&mut root, "/images1");
    add_path(&mut root, "/images2");
    add_path(&mut root, "/apples");
    add_path(&mut root, "/app/les");
    add_path(&mut root, "/apples1");
    add_path(&mut root, "/appeasement");
    add_path(&mut root, "/appealing");
    add_path(&mut root, "/date/\\:year/\\:month");
    add_path(&mut root, "/date/:year/:month");
    add_path(&mut root, "/date/:year/month");
    add_path(&mut root, "/date/:year/:month/abc");
    add_path(&mut root, "/date/:year/:month/:post");
    add_path(&mut root, "/date/:year/:month/*post");
    add_path(&mut root, "/:page");
    add_path(&mut root, "/:page/:index");
    add_path(&mut root, "/post/:post/page/:page");
    add_path(&mut root, "/plaster");
    add_path(&mut root, "/users/:pk/:related");
    add_path(&mut root, "/users/:id/updatePassword");
    add_path(&mut root, "/:something/abc");
    add_path(&mut root, "/:something/def");
    add_path(&mut root, "/apples/ab:cde/:fg/*hi");
    add_path(&mut root, "/apples/ab*cde/:fg/*hi");
    add_path(&mut root, "/apples/ab\\*cde/:fg/*hi");
    add_path(&mut root, "/apples/ab*dde");

    test_path(
      &root,
      "/users/abc/updatePassword",
      "/users/:id/updatePassword",
      Params(vec![("id", "abc").into()]),
    )
    .await;
    test_path(
      &root,
      "/users/all/something",
      "/users/:pk/:related",
      Params(vec![("pk", "all").into(), ("related", "something").into()]),
    )
    .await;

    test_path(
      &root,
      "/aaa/abc",
      "/:something/abc",
      Params(vec![("something", "aaa").into()]),
    )
    .await;
    test_path(
      &root,
      "/aaa/def",
      "/:something/def",
      Params(vec![("something", "aaa").into()]),
    )
    .await;

    test_path(&root, "/paper", "/:page", Params(vec![("page", "paper").into()])).await;

    test_path(&root, "/", "/", Params::default()).await;
    test_path(&root, "/i", "/i", Params::default()).await;
    test_path(&root, "/images", "/images", Params::default()).await;
    test_path(&root, "/images/abc.jpg", "/images/abc.jpg", Params::default()).await;
    test_path(
      &root,
      "/images/something",
      "/images/:imgname",
      Params(vec![("imgname", "something").into()]),
    )
    .await;
    test_path(
      &root,
      "/images/long/path",
      "/images/*path",
      Params(vec![("path", "long/path").into()]),
    )
    .await;
    test_path(
      &root,
      "/images/even/longer/path",
      "/images/*path",
      Params(vec![("path", "even/longer/path").into()]),
    )
    .await;
    test_path(&root, "/ima", "/ima", Params::default()).await;
    test_path(&root, "/apples", "/apples", Params::default()).await;
    test_path(&root, "/app/les", "/app/les", Params::default()).await;
    test_path(&root, "/abc", "/:page", Params(vec![("page", "abc").into()])).await;
    test_path(
      &root,
      "/abc/100",
      "/:page/:index",
      Params(vec![("page", "abc").into(), ("index", "100").into()]),
    )
    .await;
    test_path(
      &root,
      "/post/a/page/2",
      "/post/:post/page/:page",
      Params(vec![("post", "a").into(), ("page", "2").into()]),
    )
    .await;
    test_path(
      &root,
      "/date/2014/5",
      "/date/:year/:month",
      Params(vec![("year", "2014").into(), ("month", "5").into()]),
    )
    .await;
    test_path(
      &root,
      "/date/2014/month",
      "/date/:year/month",
      Params(vec![("year", "2014").into()]),
    )
    .await;
    test_path(
      &root,
      "/date/2014/5/abc",
      "/date/:year/:month/abc",
      Params(vec![("year", "2014").into(), ("month", "5").into()]),
    )
    .await;
    test_path(
      &root,
      "/date/2014/5/def",
      "/date/:year/:month/:post",
      Params(vec![
        ("year", "2014").into(),
        ("month", "5").into(),
        ("post", "def").into(),
      ]),
    )
    .await;
    test_path(
      &root,
      "/date/2014/5/def/hij",
      "/date/:year/:month/*post",
      Params(vec![
        ("year", "2014").into(),
        ("month", "5").into(),
        ("post", "def/hij").into(),
      ]),
    )
    .await;
    test_path(
      &root,
      "/date/2014/5/def/hij/",
      "/date/:year/:month/*post",
      Params(vec![
        ("year", "2014").into(),
        ("month", "5").into(),
        ("post", "def/hij/").into(),
      ]),
    )
    .await;

    test_path(
      &root,
      "/date/2014/ab%2f",
      "/date/:year/:month",
      Params(vec![("year", "2014").into(), ("month", "ab/").into()]),
    )
    .await;
    test_path(
      &root,
      "/post/ab%2fdef/page/2%2f",
      "/post/:post/page/:page",
      Params(vec![("post", "ab/def").into(), ("page", "2/").into()]),
    )
    .await;

    // Test paths with escaped wildcard characters.
    test_path(&root, "/images/*path", "/images/\\*path", Params::default()).await;
    test_path(&root, "/images/*patch", "/images/\\*patch", Params::default()).await;
    test_path(&root, "/date/:year/:month", "/date/\\:year/\\:month", Params::default()).await;
    test_path(
      &root,
      "/apples/ab*cde/lala/baba/dada",
      "/apples/ab*cde/:fg/*hi",
      Params(vec![("fg", "lala").into(), ("hi", "baba/dada").into()]),
    )
    .await;
    test_path(
      &root,
      "/apples/ab\\*cde/lala/baba/dada",
      "/apples/ab\\*cde/:fg/*hi",
      Params(vec![("fg", "lala").into(), ("hi", "baba/dada").into()]),
    )
    .await;
    test_path(
      &root,
      "/apples/ab:cde/:fg/*hi",
      "/apples/ab:cde/:fg/*hi",
      Params(vec![("fg", ":fg").into(), ("hi", "*hi").into()]),
    )
    .await;
    test_path(
      &root,
      "/apples/ab*cde/:fg/*hi",
      "/apples/ab*cde/:fg/*hi",
      Params(vec![("fg", ":fg").into(), ("hi", "*hi").into()]),
    )
    .await;
    test_path(
      &root,
      "/apples/ab*cde/one/two/three",
      "/apples/ab*cde/:fg/*hi",
      Params(vec![("fg", "one").into(), ("hi", "two/three").into()]),
    )
    .await;
    test_path(&root, "/apples/ab*dde", "/apples/ab*dde", Params::default()).await;

    test_path(&root, "/ima/bcd/fgh", "", Params::default()).await;
    test_path(&root, "/date/2014//month", "", Params::default()).await;
    test_path(&root, "/date/2014/05/", "", Params::default()).await; // Empty catchall should not match
    test_path(&root, "/post//abc/page/2", "", Params::default()).await;
    test_path(&root, "/post/abc//page/2", "", Params::default()).await;
    test_path(&root, "/post/abc/page//2", "", Params::default()).await;
    test_path(&root, "//post/abc/page/2", "", Params::default()).await;
    test_path(&root, "//post//abc//page//2", "", Params::default()).await;
  }

  async fn test_path(
    node: &Node<'static, ReqHandler>,
    path: &'static str,
    expected_path: &'static str,
    expected_params: Params<'static>,
  ) {
    let mtc = node.search(&Method::GET, path[1..].into());
    if !expected_path.is_empty() && mtc.is_none() {
      panic!(
        "No match for {}, expected {}\n{}",
        path,
        expected_path,
        node.dump_tree("", "")
      )
    } else if expected_path.is_empty() && mtc.is_some() {
      panic!(
        "Expected no match for {} but got {:?} with params {:?}.\nNode and subtree was\n{}",
        path,
        mtc.map(|v| v.path),
        expected_params,
        node.dump_tree("", "")
      );
    }

    if mtc.is_none() {
      return;
    }

    let mtc = mtc.unwrap();

    let handler = mtc.value;
    if handler.is_none() {
      panic!(
        "Path {} returned a node without a handler.\nNode and subtree was\n{}",
        path,
        node.dump_tree("", "")
      );
    }

    let handler = handler.unwrap();
    let req = Request::builder()
      .extension(Params::default())
      .body(Body::empty())
      .unwrap();
    let response = handler(req);
    let matched_path = String::from_utf8(
      async { hyper::body::to_bytes(response.into_body()).await }
        .await
        .unwrap()
        .to_vec(),
    )
    .unwrap();

    if matched_path != expected_path {
      panic!(
        "Path {} matched {}, expected {}.\nNode and subtree was\n{}",
        path,
        matched_path,
        expected_path,
        node.dump_tree("", "")
      )
    }

    if expected_params.is_empty() {
      if !mtc.params.is_empty() {
        panic!("Path {} expected no parameters, saw {:?}", path, mtc.params);
      }
    } else {
      if expected_params.len() > mtc.params.len() {
        panic!(
          "Got {} params back but node specifies {}",
          expected_params.len(),
          mtc.params.len()
        );
      }

      let params = mtc.parameters();
      assert_eq!(expected_params, params, "expected_path={}", expected_path);
    }
  }

  #[test]
  #[should_panic]
  fn test_wildcard_mismatch() {
    let mut n: Node<()> = Node {
      path: "/".into(),
      ..Default::default()
    };
    n.leaf_wildcard_names = Some(vec!["first".into(), "third".into()]);

    n.add_path(
      "".into(),
      Some(vec!["first".into(), "second".into()]),
      false,
      Handler {
        verb: Method::GET,
        value: (),
        implicit_head: false,
        add_slash: false,
      },
    );
  }

  #[test]
  #[should_panic]
  fn test_panics_catch_all_trailing_slash() {
    let mut n: Node<()> = Node::new();

    n.add_path(
      "abc/*path/".into(),
      None,
      false,
      Handler {
        verb: Method::GET,
        value: (),
        implicit_head: false,
        add_slash: false,
      },
    );
  }

  #[test]
  #[should_panic]
  fn test_panics_catch_all_conflict() {
    let mut n: Node<()> = Node::new();

    n.add_path(
      "abc/*path".into(),
      None,
      false,
      Handler {
        verb: Method::GET,
        value: (),
        implicit_head: false,
        add_slash: false,
      },
    );
    n.add_path(
      "abc/*paths".into(),
      None,
      false,
      Handler {
        verb: Method::GET,
        value: (),
        implicit_head: false,
        add_slash: false,
      },
    );
  }

  #[test]
  #[should_panic]
  fn test_panics_catch_all_extra_segment() {
    let mut n: Node<()> = Node::new();

    n.add_path(
      "abc/*path/def".into(),
      None,
      false,
      Handler {
        verb: Method::GET,
        value: (),
        implicit_head: false,
        add_slash: false,
      },
    );
  }

  #[test]
  fn test_wildcard_success() {
    let mut n: Node<()> = Node { ..Default::default() };
    let wildcards = Some(vec!["first".into(), "second".into()]);
    n.add_path(
      "".into(),
      wildcards.clone(),
      false,
      Handler {
        verb: Method::GET,
        value: (),
        implicit_head: false,
        add_slash: false,
      },
    );
    assert_eq!(n.leaf_wildcard_names, wildcards);
  }
}
