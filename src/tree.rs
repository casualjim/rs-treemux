use hyper::Method;
use percent_encoding::percent_decode_str;
use std::{borrow::Cow, collections::HashMap, fmt::Debug, iter::FromIterator, ops::Index, vec};

/// The response returned when getting the value for a specific path with
/// [`Node::search`](crate::tree::Node::search)
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
  pub is_catch_all: bool,
  /// The route parameters. See [parameters](/index.html#parameters) for more details.
  param_names: Vec<Cow<'a, str>>,
  pub leaf_handler: &'a HashMap<Method, V>,
  /// The route parameters. See [parameters](/index.html#parameters) for more details.
  pub parameters: Params,
}

impl<'a, V> Match<'a, V> {
  /// The route parameters. See [parameters](/index.html#parameters) for more details.
  pub fn update_parameters(&mut self) {
    self.parameters = self.param_names.clone().into_iter().zip(self.params.clone()).collect();
  }
}

/// Param is a single URL parameter, consisting of a key and a value.
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
  pub key: String,
  pub value: String,
}

impl Param {
  pub fn new(key: String, value: String) -> Self {
    Self { key, value }
  }
}

impl<'a> From<(&'a str, &'a str)> for Param {
  fn from(input: (&'a str, &'a str)) -> Self {
    Param::new(input.0.into(), input.1.into())
  }
}

impl<'a, 'b> From<(&'a Cow<'b, str>, &'a Cow<'b, str>)> for Param
where
  'b: 'a,
{
  fn from(input: (&'a Cow<'b, str>, &'a Cow<'b, str>)) -> Self {
    Param::new(input.0.to_string(), input.1.to_string())
  }
}

impl<'a> From<(Cow<'a, str>, Cow<'a, str>)> for Param {
  fn from(input: (Cow<'a, str>, Cow<'a, str>)) -> Self {
    Param::new(input.0.to_string(), input.1.to_string())
  }
}

/// A `Vec` of `Param` returned by a route match.
/// There are two ways to retrieve the value of a parameter:
///  1) by the name of the parameter
/// ```rust
///  # use treemux::Params;
///  # let params = Params::default();
///
///  let user = params.get("user"); // defined by :user or *user
/// ```
///  2) by the index of the parameter. This way you can also get the name (key)
/// ```rust,no_run
///  # use treemux::Params;
///  # let params = Params::default();
///  let third_key = &params[2].key;   // the name of the 3rd parameter
///  let third_value = &params[2].value; // the value of the 3rd parameter
/// ```
#[derive(Debug, PartialEq, Clone)]
pub struct Params(pub Vec<Param>);

impl<'a> Default for Params {
  fn default() -> Self {
    Self(Vec::new())
  }
}

impl<'a> Index<usize> for Params {
  type Output = Param;

  #[inline]
  fn index(&self, i: usize) -> &Param {
    &self.0[i]
  }
}

impl<'a> std::ops::IndexMut<usize> for Params {
  fn index_mut(&mut self, i: usize) -> &mut Param {
    &mut self.0[i]
  }
}

impl<'a> FromIterator<(&'a str, &'a str)> for Params {
  fn from_iter<T: IntoIterator<Item = (&'a str, &'a str)>>(iter: T) -> Self {
    Params(iter.into_iter().map(Into::into).collect())
  }
}

impl<'a, 'b> FromIterator<(&'a Cow<'b, str>, &'a Cow<'b, str>)> for Params
where
  'b: 'a,
{
  fn from_iter<T: IntoIterator<Item = (&'a Cow<'b, str>, &'a Cow<'b, str>)>>(iter: T) -> Self {
    Params(iter.into_iter().map(Into::into).collect())
  }
}

impl<'a> FromIterator<(Cow<'a, str>, Cow<'a, str>)> for Params {
  fn from_iter<T: IntoIterator<Item = (Cow<'a, str>, Cow<'a, str>)>>(iter: T) -> Self {
    Params(iter.into_iter().map(Into::into).collect())
  }
}

impl Params {
  /// Returns the value of the first `Param` whose key matches the given name.
  pub fn get<K: AsRef<str>>(&self, name: K) -> Option<&str> {
    match self.0.iter().find(|param| param.key == name.as_ref()) {
      Some(param) => Some(&param.value),
      None => None,
    }
  }

  pub fn is_empty(&self) -> bool {
    self.0.is_empty()
  }

  /// Inserts a URL parameter into the vector
  pub fn push<P: Into<Param>>(&mut self, p: P) {
    self.0.push(p.into());
  }

  pub fn len(&self) -> usize {
    self.0.len()
  }

  pub fn first(&self) -> Option<&Param> {
    self.0.first()
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
struct Handler<V> {
  method: Method,
  value: V,
  implicit_head: bool,
  head_can_use_get: bool,
  add_slash: bool,
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

#[derive(Debug, Default)]
pub struct HandlerConfig<'a, V> {
  pub method: Method,
  pub path: Cow<'a, str>,
  pub implicit_head: bool,
  pub head_can_use_get: bool,
  pub add_slash: bool,
  pub value: V,
}

fn strip_start_slash(path: Cow<str>) -> Cow<str> {
  if path.starts_with('/') {
    path.strip_prefix('/').unwrap().to_string().into()
  } else {
    path
  }
}

impl<'a, V> HandlerConfig<'a, V> {
  pub fn new(method: Method, path: Cow<'a, str>, value: V) -> HandlerConfig<'a, V> {
    HandlerConfig {
      method,
      path,
      implicit_head: false,
      add_slash: false,
      head_can_use_get: false,
      value,
    }
  }

  fn stripped_path(&self) -> Cow<'a, str> {
    strip_start_slash(self.path.clone())
  }
}

type IdxTuple = (usize, char);
type IdxVec = Vec<IdxTuple>;

impl<'a, V> Node<'a, V>
where
  V: Clone,
{
  pub fn new() -> Self {
    Node {
      path: "/".into(),
      ..Default::default()
    }
  }

  pub fn insert(&mut self, config: HandlerConfig<'a, V>) {
    self.add_path(
      config.stripped_path(),
      None,
      false,
      Handler {
        method: config.method,
        value: config.value,
        implicit_head: config.implicit_head,
        head_can_use_get: config.head_can_use_get,
        add_slash: config.add_slash,
      },
    );
  }

  pub fn search<'b, P: AsRef<str>>(&'b self, method: &'b Method, path: P) -> Option<Match<'b, V>> {
    let pth: Cow<str> = path.as_ref().to_string().into();
    self
      .internal_search(method, strip_start_slash(pth.clone()), pth)
      .map(|mut v| {
        v.update_parameters();
        v
      })
  }

  fn internal_search<'b>(
    &'b self,
    method: &'b Method,
    path: Cow<'b, str>,
    original_path: Cow<'b, str>,
  ) -> Option<Match<'b, V>> {
    let path_len = path.len();
    if path.is_empty() {
      if self.leaf_handler.is_empty() {
        return None;
      }

      return Some(Match {
        value: self.leaf_handler.get(method),
        params: vec![],
        path,
        pattern: self.path.clone(),
        param_names: self.leaf_wildcard_names.clone().unwrap_or_default(),
        implicit_head: self.implicit_head,
        add_slash: self.add_slash,
        leaf_handler: &self.leaf_handler,
        parameters: Params::default(),
        is_catch_all: self.is_catch_all,
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
          found = child.internal_search(method, next_path, original_path.clone());
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
        let wc_match = wildcard_child.internal_search(method, next_token, original_path);

        if wc_match.as_ref().filter(|v| v.value.is_some()).is_some() || (found.is_none() && wc_match.is_some()) {
          let pth = percent_decode_str(this_token.as_ref()).decode_utf8_lossy().to_string();

          if let Some(mut the_match) = wc_match {
            let mut nwparams = vec![pth.into()];
            nwparams.append(&mut the_match.params.to_vec());
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
      let handler = catch_all_child.leaf_handler.get(&method);

      // Found a handler, or we found a catchall node without a handler.
      // Either way, return it since there's nothing left to check after this.
      if handler.is_some() || found.is_none() {
        let pth = percent_decode_str(path.as_ref()).decode_utf8_lossy().to_string();
        return Some(Match {
          value: handler,
          params: vec![pth.clone().into()],
          pattern: pth.into(),
          param_names: catch_all_child.leaf_wildcard_names.clone().unwrap_or_default(),
          path: self.path.clone(),
          add_slash: catch_all_child.add_slash,
          implicit_head: catch_all_child.implicit_head,
          leaf_handler: &catch_all_child.leaf_handler,
          parameters: Params::default(),
          is_catch_all: catch_all_child.is_catch_all,
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

  fn set_handler(&mut self, method: Method, handler: V, implicit_head: bool, add_slash: bool) {
    if self.leaf_handler.contains_key(&method) && (method != Method::HEAD || !self.implicit_head) {
      panic!("{} already handles {}", self.path, method)
    }

    if method == Method::HEAD {
      self.implicit_head = implicit_head;
    }
    self.add_slash = add_slash;
    self.leaf_handler.insert(method, handler);
  }

  pub fn extend(&mut self, path: Cow<'a, str>, node: Node<'a, V>) {
    self.internal_add_node(strip_start_slash(path), None, false, node, true)
  }

  fn add_wildcards_to_leafs(&mut self, wildcards: &[Cow<'a, str>]) {
    for n in self.static_child.iter_mut().map(|v| v.as_mut()) {
      if let Some(n) = n {
        if !n.leaf_handler.is_empty() {
          n.leaf_wildcard_names = Some(wildcards.to_vec());
        } else {
          n.add_wildcards_to_leafs(wildcards)
        }
      }
    }

    if let Some(wc) = self.wildcard_child.as_mut() {
      if !wc.leaf_handler.is_empty() {
        wc.leaf_wildcard_names = Some(wildcards.to_vec());
      }
      wc.add_wildcards_to_leafs(wildcards)
    }
  }

  const JUST_SLASH: &'static [char] = &['/'];
  fn internal_add_node(
    &mut self,
    path: Cow<'a, str>,
    wildcards: Option<Vec<Cow<'a, str>>>,
    in_static_token: bool,
    mut node: Node<'a, V>,
    is_first: bool,
  ) {
    if path.is_empty() {
      // When we found a place to attach the new node, we need to make sure
      // that we move the handlers from the root node in the new node at '/'
      // to the new root node as if it was explicitly defined
      if node.path == "/" && self.static_indices == Self::JUST_SLASH {
        self.leaf_handler.extend(std::mem::take(&mut node.leaf_handler));
      }
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
          node.add_wildcards_to_leafs(wildcards);
        }
      }
      if let Some(idx) = self.static_indices.iter().position(|c| c == &'/') {
        if let Some(mut previous) = self.static_child[idx].replace(Box::new(node)) {
          let n = self.static_child[idx].as_mut().unwrap();
          let (known, unknown): (IdxVec, IdxVec) = previous
            .static_indices
            .iter()
            .copied()
            .enumerate()
            .partition(|&c| n.static_indices.contains(&c.1));

          for (fidx, c) in unknown {
            n.static_indices.push(c);
            n.static_child.push(previous.static_child[fidx].take());
          }

          for (fidx, c) in known {
            let nidx = n.static_indices.iter().position(|cc| cc == &c).unwrap();
            let prev = previous.static_child[fidx].as_mut().unwrap();
            n.static_child[nidx]
              .as_ref()
              .unwrap()
              .leaf_handler
              .iter()
              .for_each(|(method, route)| {
                prev.leaf_handler.insert(method.clone(), route.clone());
              });
            n.static_child[nidx].as_mut().unwrap().leaf_handler = prev.leaf_handler.clone();
          }
        }
      } else {
        if let Some(ref wildcards) = wildcards {
          // Make sure the current wildcards are the same as the old ones.
          // When they aren't, we have an ambiguous path
          if let Some(leaf_wildcards) = &node.leaf_wildcard_names {
            if wildcards.len() != leaf_wildcards.len() {
              // this should never happen, they said
              panic!("Reached leaf node with differing wildcard slice length. Please report this as a bug.")
            }

            if wildcards != leaf_wildcards {
              panic!("Wildcards {:?} are ambiguous with {:?}.", leaf_wildcards, wildcards);
            }
          } else {
            self.leaf_wildcard_names = Some(wildcards.to_vec());
          }
        }

        self.leaf_handler.extend(std::mem::take(&mut node.leaf_handler));
        self.static_child.push(Some(Box::new(node)));
        self.static_indices.push('/');
      }

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

    if c == '*' {
      panic!("it's not allowed to add catch alls as a group")
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

      self.wildcard_child.as_mut().map(|ch| {
        ch.internal_add_node(remaining_path, wil, false, node, false);
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
              child.internal_add_node(
                (&path[prefix_split..]).to_string().into(),
                wildcards,
                in_static_token,
                node,
                is_first,
              );
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
      self.static_indices.append(&mut vec![c]);
      child.internal_add_node(remaining_path, wildcards, in_static_token, node, false);
      let chld = Some(Box::new(child));
      self.static_child.append(&mut vec![chld]);
    }
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
      self.set_handler(handler.method.clone(), handler.value.clone(), false, handler.add_slash);
      if handler.head_can_use_get && handler.method == Method::GET && !self.leaf_handler.contains_key(&Method::HEAD) {
        self.set_handler(Method::HEAD, handler.value, true, handler.add_slash);
      }
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
        n.set_handler(handler.method.clone(), handler.value.clone(), false, handler.add_slash);
        if handler.head_can_use_get && handler.method == Method::GET && !n.leaf_handler.contains_key(&Method::HEAD) {
          n.set_handler(Method::HEAD, handler.value, true, handler.add_slash);
        }
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

  use std::{panic, sync::Arc, vec};

  use super::{Handler, HandlerConfig, Node, Params};

  type ReqHandler = Arc<dyn Fn(Request<Body>) -> Response<Body>>;

  fn make_root_node<'a>() -> Node<'a, ReqHandler> {
    let mut root = Node::new();
    add_path(&mut root, "/", "root-router");
    add_method_path(&mut root, Method::POST, "/", "root-router");
    add_path(&mut root, "/images", "root-router");
    add_path(&mut root, "/images/abc.jpg", "root-router");
    add_path(&mut root, "/images/:imgname", "root-router");
    add_path(&mut root, "/images/\\*path", "root-router");
    add_path(&mut root, "/images/\\*patch", "root-router");
    add_path(&mut root, "/images/*path", "root-router");
    add_path(&mut root, "/appcenter", "root-router");
    add_path(&mut root, "/apps", "root-router");
    add_path(&mut root, "/apps/inventory", "root-router");
    add_method_path(&mut root, Method::POST, "/apps/inventory", "root-router");
    add_path(&mut root, "/apps/settings/default", "root-router");
    add_path(&mut root, "/post/:post/page/:page", "root-router");
    root
  }
  #[tokio::test]
  async fn test_merge_node_no_overlap() {
    let mut root = make_root_node();

    // no overlap with existing paths, so just inserts
    let mut sub1 = Node::new();
    add_path(&mut sub1, "/", "sub1-router");
    add_path(&mut sub1, "/approve", "sub1-router");
    add_path(&mut sub1, "/unapprove", "sub1-router");
    root.extend("/api".into(), sub1);

    test_path_ext(
      &root,
      &Method::GET,
      "/api/approve",
      "/approve",
      "sub1-router",
      Params::default(),
    )
    .await;
    test_path_ext(
      &root,
      &Method::GET,
      "/apps/inventory",
      "/apps/inventory",
      "root-router",
      Params::default(),
    )
    .await;
  }
  #[tokio::test]
  async fn test_merge_node_simple_overlap() {
    let mut root = make_root_node();
    // overlaps with existing paths, this overrides (Method, Path) entries that previously existed
    let mut sub2 = Node::new();
    add_path(&mut sub2, "/", "sub2-router");
    add_path(&mut sub2, "/search", "sub2-router");
    add_path(&mut sub2, "/settings/other", "sub2-router");
    add_method_path(&mut sub2, Method::POST, "/inventory", "sub2-router");

    root.extend("/apps".into(), sub2);

    test_path_ext(&root, &Method::GET, "/apps", "/", "sub2-router", Params::default()).await;
    test_path_ext(
      &root,
      &Method::GET,
      "/apps/search",
      "/search",
      "sub2-router",
      Params::default(),
    )
    .await;
    test_path_ext(
      &root,
      &Method::GET,
      "/apps/inventory",
      "/apps/inventory",
      "root-router",
      Params::default(),
    )
    .await;
    test_path_ext(
      &root,
      &Method::POST,
      "/apps/inventory",
      "/inventory",
      "sub2-router",
      Params::default(),
    )
    .await;
  }

  #[tokio::test]
  async fn test_merge_node_no_overlap_root() {
    let mut root = make_root_node();
    let mut sub2 = Node::new();
    add_path(&mut sub2, "/search", "sub2-router");
    add_path(&mut sub2, "/settings/other", "sub2-router");
    add_method_path(&mut sub2, Method::POST, "/inventory", "sub2-router");

    root.extend("/apps".into(), sub2);

    test_path_ext(&root, &Method::GET, "/apps", "/apps", "root-router", Params::default()).await;
    test_path_ext(
      &root,
      &Method::GET,
      "/apps/search",
      "/search",
      "sub2-router",
      Params::default(),
    )
    .await;
    test_path_ext(
      &root,
      &Method::GET,
      "/apps/inventory",
      "/apps/inventory",
      "root-router",
      Params::default(),
    )
    .await;
    test_path_ext(
      &root,
      &Method::POST,
      "/apps/inventory",
      "/inventory",
      "sub2-router",
      Params::default(),
    )
    .await;
  }

  #[tokio::test]
  async fn test_merge_node_route_params_no_overlap() {
    let mut root = make_root_node();
    let mut sub3 = Node::new();
    add_path(&mut sub3, "/", "sub3-router");
    add_path(&mut sub3, "/approve", "sub3-router");
    add_path(&mut sub3, "/unapprove", "sub3-router");
    root.extend("/blog/post/:post/page/:page".into(), sub3);

    test_path_ext(
      &root,
      &Method::GET,
      "/blog/post/4254/page/3/approve",
      "/approve",
      "sub3-router",
      Params(vec![("post", "4254").into(), ("page", "3").into()]),
    )
    .await;
    test_path_ext(
      &root,
      &Method::GET,
      "/blog/post/4254/page/3",
      "/",
      "sub3-router",
      Params(vec![("post", "4254").into(), ("page", "3").into()]),
    )
    .await;
  }
  #[tokio::test]
  async fn test_merge_node_route_params_overlap() {
    let mut root = make_root_node();
    let mut sub4 = Node::new();
    add_path(&mut sub4, "/", "sub4-router");
    add_path(&mut sub4, "/approve", "sub4-router");
    add_path(&mut sub4, "/unapprove", "sub4-router");
    root.extend("/post/:post/page/:page".into(), sub4);

    test_path_ext(
      &root,
      &Method::GET,
      "/post/4254/page/3/approve",
      "/approve",
      "sub4-router",
      Params(vec![("post", "4254").into(), ("page", "3").into()]),
    )
    .await;
    test_path_ext(
      &root,
      &Method::GET,
      "/post/4254/page/3",
      "/",
      "sub4-router",
      Params(vec![("post", "4254").into(), ("page", "3").into()]),
    )
    .await;
  }
  #[tokio::test]
  async fn test_merge_node_route_params_root_overlap() {
    let mut root = make_root_node();
    let mut sub4 = Node::new();
    add_path(&mut sub4, "/approve", "sub4-router");
    add_path(&mut sub4, "/unapprove", "sub4-router");
    root.extend("/post/:post/page/:page".into(), sub4);

    test_path_ext(
      &root,
      &Method::GET,
      "/post/4254/page/3/approve",
      "/approve",
      "sub4-router",
      Params(vec![("post", "4254").into(), ("page", "3").into()]),
    )
    .await;
    test_path_ext(
      &root,
      &Method::GET,
      "/post/4254/page/3",
      "/post/:post/page/:page",
      "root-router",
      Params(vec![("post", "4254").into(), ("page", "3").into()]),
    )
    .await;
  }

  fn add_path(node: &mut Node<'static, ReqHandler>, path: &'static str, response_header: &'static str) {
    add_method_path(node, Method::GET, path, response_header)
  }

  fn add_method_path(
    node: &mut Node<'static, ReqHandler>,
    method: Method,
    path: &'static str,
    response_header: &'static str,
  ) {
    debug!("adding path path={}", path);
    node.insert(HandlerConfig {
      method,
      path: path.into(),
      implicit_head: false,
      add_slash: false,
      head_can_use_get: false,
      value: Arc::new(move |mut req: Request<Body>| {
        let params = req.extensions_mut().get_mut::<Params>().unwrap();
        params.push(("path", path));
        Response::builder()
          .status(StatusCode::OK)
          .header("X-Passed", response_header.to_string())
          .body(Body::from(path))
          .unwrap()
      }),
    });
  }

  #[tokio::test]
  async fn test_tree() {
    std::env::set_var("RUST_LOG", "debug");

    let mut root = Node::new();
    add_path(&mut root, "/", "root-router");
    add_path(&mut root, "/i", "root-router");
    add_path(&mut root, "/i/:aaa", "root-router");
    add_path(&mut root, "/images", "root-router");
    add_path(&mut root, "/images/abc.jpg", "root-router");
    add_path(&mut root, "/images/:imgname", "root-router");
    add_path(&mut root, "/images/\\*path", "root-router");
    add_path(&mut root, "/images/\\*patch", "root-router");
    add_path(&mut root, "/images/*path", "root-router");
    add_path(&mut root, "/ima", "root-router");
    add_path(&mut root, "/ima/:par", "root-router");
    add_path(&mut root, "/images1", "root-router");
    add_path(&mut root, "/images2", "root-router");
    add_path(&mut root, "/apples", "root-router");
    add_path(&mut root, "/app/les", "root-router");
    add_path(&mut root, "/apples1", "root-router");
    add_path(&mut root, "/appeasement", "root-router");
    add_path(&mut root, "/appealing", "root-router");
    add_path(&mut root, "/date/\\:year/\\:month", "root-router");
    add_path(&mut root, "/date/:year/:month", "root-router");
    add_path(&mut root, "/date/:year/month", "root-router");
    add_path(&mut root, "/date/:year/:month/abc", "root-router");
    add_path(&mut root, "/date/:year/:month/:post", "root-router");
    add_path(&mut root, "/date/:year/:month/*post", "root-router");
    add_path(&mut root, "/:page", "root-router");
    add_path(&mut root, "/:page/:index", "root-router");
    add_path(&mut root, "/post/:post/page/:page", "root-router");
    add_path(&mut root, "/plaster", "root-router");
    add_path(&mut root, "/users/:pk/:related", "root-router");
    add_path(&mut root, "/users/:id/updatePassword", "root-router");
    add_path(&mut root, "/:something/abc", "root-router");
    add_path(&mut root, "/:something/def", "root-router");
    add_path(&mut root, "/apples/ab:cde/:fg/*hi", "root-router");
    add_path(&mut root, "/apples/ab*cde/:fg/*hi", "root-router");
    add_path(&mut root, "/apples/ab\\*cde/:fg/*hi", "root-router");
    add_path(&mut root, "/apples/ab*dde", "root-router");

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
    expected_params: Params,
  ) {
    test_path_ext(node, &Method::GET, path, expected_path, "root-router", expected_params).await
  }
  async fn test_path_ext(
    node: &Node<'static, ReqHandler>,
    method: &'static Method,
    path: &'static str,
    expected_path: &'static str,
    expected_header: &'static str,
    expected_params: Params,
  ) {
    let mtc = node.search(method, path);
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
    let actual_header = String::from_utf8(response.headers().get("X-Passed").unwrap().as_ref().to_vec()).unwrap();
    let matched_path = String::from_utf8(hyper::body::to_bytes(response.into_body()).await.unwrap().to_vec()).unwrap();

    assert_eq!(
      matched_path,
      expected_path,
      "Path {} matched {}, expected {}.\nNode and subtree was\n{}",
      path,
      matched_path,
      expected_path,
      node.dump_tree("", "")
    );

    assert_eq!(
      actual_header,
      expected_header,
      "header {} matched {}, expected {}.\nNode and subtree was\n{}",
      path,
      actual_header,
      expected_header,
      node.dump_tree("", "")
    );

    if expected_params.is_empty() {
      assert!(
        mtc.params.is_empty(),
        "Path {} expected no parameters, saw {:?}",
        path,
        mtc.params
      );
    } else {
      assert!(
        expected_params.len() <= mtc.params.len(),
        "Got {} params back but node specifies {}",
        expected_params.len(),
        mtc.params.len()
      );

      let params = mtc.parameters;
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
        method: Method::GET,
        value: (),
        implicit_head: false,
        head_can_use_get: false,
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
        method: Method::GET,
        value: (),
        implicit_head: false,
        head_can_use_get: false,
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
        method: Method::GET,
        value: (),
        implicit_head: false,
        head_can_use_get: false,
        add_slash: false,
      },
    );
    n.add_path(
      "abc/*paths".into(),
      None,
      false,
      Handler {
        method: Method::GET,
        value: (),
        implicit_head: false,
        head_can_use_get: false,
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
        method: Method::GET,
        value: (),
        implicit_head: false,
        head_can_use_get: false,
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
        method: Method::GET,
        value: (),
        implicit_head: false,
        head_can_use_get: false,
        add_slash: false,
      },
    );
    assert_eq!(n.leaf_wildcard_names, wildcards);
  }
}
