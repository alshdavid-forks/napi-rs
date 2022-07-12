use serde::Deserialize;
use std::fmt::Write as _;
use std::{cmp::Ordering, collections::HashMap, fs::File, io::Read, path::Path};

pub fn correct_str_indent(src: &str, indent: usize) -> String {
  let mut bracket_depth = 0;
  let mut result = src
    .lines()
    .into_iter()
    .map(|line| {
      let line = line.trim();
      if line.is_empty() {
        return String::new();
      }

      let is_in_multiline_comment = line.starts_with('*');
      let is_closing_bracket = line.ends_with('}');
      let is_opening_bracket = line.ends_with('{');

      let right_indent = indent
        + if is_opening_bracket && !is_in_multiline_comment {
          bracket_depth += 1;
          (bracket_depth - 1) * 2
        } else {
          if is_closing_bracket && bracket_depth > 0 && !is_in_multiline_comment {
            bracket_depth -= 1;
          }
          bracket_depth * 2
        }
        + if is_in_multiline_comment { 1 } else { 0 };

      let s = format!("{}{}", " ".repeat(right_indent), line);

      s
    })
    .collect::<Vec<_>>()
    .join("\n");

  // String.lines() will eat posting lines.
  if src.ends_with('\n') {
    result.push('\n');
  }

  result
}

#[derive(Deserialize, Debug)]
enum TypeDefKind {
  #[serde(rename = "const")]
  Const,
  #[serde(rename = "fn")]
  Fn,
  #[serde(rename = "struct")]
  Struct,
  #[serde(rename = "impl")]
  Impl,
  #[serde(rename = "enum")]
  Enum,
  #[serde(rename = "interface")]
  Interface,
}

#[derive(Deserialize, Debug)]
struct TypeDefLine {
  kind: TypeDefKind,
  name: String,
  original_name: Option<String>,
  def: String,
  js_doc: Option<String>,
  js_mod: Option<String>,
}

impl TypeDefLine {
  fn into_pretty_print(self, indent: usize) -> String {
    let s = match self.kind {
      TypeDefKind::Interface => format!(
        "{}export interface {} {{\n{}\n}}\n\n",
        self.js_doc.unwrap_or_default(),
        self.name,
        &self.def,
      ),
      TypeDefKind::Enum => format!(
        "{}export const enum {} {{\n{}\n}}\n\n",
        self.js_doc.unwrap_or_default(),
        self.name,
        &self.def
      ),
      TypeDefKind::Struct => {
        let mut s = format!(
          "{}export class {} {{\n{}\n}}",
          self.js_doc.unwrap_or_default(),
          self.name,
          self.def,
        );

        match self.original_name {
          Some(original_name) if original_name != self.name => {
            s.push('\n');
            let _ = write!(s, "export type {} = {}\n\n", original_name, self.name);
          }
          _ => {
            s.push_str("\n\n");
          }
        }

        s
      }
      _ => format!("{}{}\n", self.js_doc.unwrap_or_default(), self.def),
    };

    correct_str_indent(&s, indent)
  }
}

pub struct IntermidiateTypeDefFile {
  defs: Vec<TypeDefLine>,
}

impl<P: AsRef<Path>> From<P> for IntermidiateTypeDefFile {
  fn from(path: P) -> Self {
    let mut defs = Vec::<TypeDefLine>::new();
    let mut content = String::new();
    File::open(path)
      .unwrap()
      .read_to_string(&mut content)
      .unwrap();

    content.lines().for_each(|line| {
      if !line.is_empty() {
        defs.push(serde_json::from_str(line).unwrap());
      }
    });

    // move all `struct` def to the very top
    // and order the rest alphabetically.
    defs.sort_by(|a, b| match a.kind {
      TypeDefKind::Struct => match b.kind {
        TypeDefKind::Struct => a.name.cmp(&b.name),
        _ => Ordering::Less,
      },
      _ => match b.kind {
        TypeDefKind::Struct => Ordering::Greater,
        _ => a.name.cmp(&b.name),
      },
    });

    IntermidiateTypeDefFile { defs }
  }
}

const TOP_LEVEL_NAMESPACE: &str = "__TOP_LEVEL__";
const TYPE_DEF_HEADER: &str = r#"/* tslint:disable */
/* eslint-disable */

/* auto-generated by NAPI-RS */

"#;

impl TypeDefLine {
  fn preprocess(lines: Vec<TypeDefLine>) -> HashMap<String, Vec<TypeDefLine>> {
    let mut namespace_groupped_lines = HashMap::<String, Vec<TypeDefLine>>::new();
    let mut class_defs = HashMap::<String, *mut TypeDefLine>::new();
    let max_group_size = lines.len();

    for line in lines.into_iter() {
      let group = namespace_groupped_lines
        .entry(
          line
            .js_mod
            .clone()
            .unwrap_or_else(|| String::from(TOP_LEVEL_NAMESPACE)),
        )
        .or_insert_with(|| {
          // we will use raw pointer magic, so we don't want the vec pointer moved.
          Vec::with_capacity(max_group_size)
        });

      match line.kind {
        TypeDefKind::Struct => {
          let name = line.name.clone();
          group.push(line);
          class_defs.insert(name, group.last_mut().unwrap() as *mut TypeDefLine);
        }
        TypeDefKind::Impl => {
          // `impl` can't have js alias
          class_defs
            .entry(line.name.clone())
            .and_modify(|&mut prev| unsafe {
              if !(*prev).def.is_empty() {
                (*prev).def.push('\n');
              }
              (*prev).def.push_str(&line.def);
            });
        }
        _ => {
          group.push(line);
        }
      }
    }

    namespace_groupped_lines
  }
}

impl IntermidiateTypeDefFile {
  pub fn into_dts(self, with_header: bool) -> Result<String, String> {
    if self.defs.is_empty() {
      return Err("No type definitions found".to_string());
    }

    let mut dts = if with_header {
      String::from(TYPE_DEF_HEADER)
    } else {
      String::new()
    };

    dts.push_str(
      r#"export class ExternalObject<T> {
  readonly '': {
    readonly '': unique symbol
    [K: symbol]: T
  }
}

"#,
    );

    let mut groupped_lines = TypeDefLine::preprocess(self.defs)
      .into_iter()
      .collect::<Vec<_>>();

    groupped_lines.sort_by_key(|g| g.0.clone());

    for (namespace, lines) in groupped_lines {
      if namespace == TOP_LEVEL_NAMESPACE {
        for line in lines {
          dts.push_str(&line.into_pretty_print(0));
        }
      } else {
        let _ = writeln!(dts, "export namespace {} {{", namespace);
        for line in lines {
          dts.push_str(&line.into_pretty_print(2));
        }
        dts.push_str("}\n");
      }
    }

    if !dts.ends_with('\n') {
      dts.push('\n')
    }

    Ok(dts)
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn should_indent_str_correctly() {
    let raw = r#"
class A {
  foo() {
    a = b
  }

bar = () => {

}
    boz = 1
  }

namespace B {
    namespace C {
type D = A
    }
}
"#;
    let expected = r#"
class A {
  foo() {
    a = b
  }

  bar = () => {

  }
  boz = 1
}

namespace B {
  namespace C {
    type D = A
  }
}
"#;
    assert_eq!(correct_str_indent(raw, 0), expected);
  }
}
