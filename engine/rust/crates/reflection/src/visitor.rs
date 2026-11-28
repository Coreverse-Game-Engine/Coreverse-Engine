//! Walking a reflected value (milestone R5).
//!
//! [`visit`] walks a value depth-first and calls a [`ReflectVisitor`] for every value it meets: the root, the
//! fields of structs, tuples and the active variant of enums, the items of lists, arrays and sets, and the
//! keys and values of maps. Each call gets a [`VisitNode`] that says where the value sits (path, depth, the
//! field it is in, and that field's [`Metadata`](crate::Metadata)), so a serializer can skip `transient`
//! fields and an editor can hide `hidden` ones without any knowledge of the concrete types.
//!
//! The walker does not serialize anything itself and does not depend on any format; formats are built on top
//! of it in the serialization crate.
//!
//! ```
//! use reflection::{Reflect, ReflectVisitor, VisitControl, VisitNode, visit};
//! use std::convert::Infallible;
//!
//! #[derive(Reflect)]
//! struct Player {
//!     name: String,
//!     scores: Vec<u32>,
//! }
//!
//! struct Paths(Vec<String>);
//!
//! impl ReflectVisitor for Paths {
//!     type Error = Infallible;
//!
//!     fn enter(
//!         &mut self,
//!         node: &VisitNode<'_>,
//!         _value: &dyn Reflect,
//!     ) -> Result<VisitControl, Infallible> {
//!         self.0.push(node.path().to_owned());
//!         Ok(VisitControl::Continue)
//!     }
//! }
//!
//! let player = Player { name: "Ada".into(), scores: vec![3, 5] };
//! let mut paths = Paths(Vec::new());
//! visit(&player, &mut paths).unwrap();
//! assert_eq!(paths.0, ["", "name", "scores", "scores[0]", "scores[1]"]);
//! ```

use std::fmt;

use crate::info::{FieldInfo, TypeInfo};
use crate::metadata::Metadata;
use crate::reflect::{Reflect, ReflectRef};

/// What the walker should do after [`ReflectVisitor::enter`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisitControl {
    /// Go on into the children of the value.
    Continue,
    /// Do not visit the children of this value; [`ReflectVisitor::leave`] is still called for it, and the walk
    /// goes on with its next sibling.
    SkipChildren,
    /// End the whole walk now, successfully. `leave` is not called for the values still open.
    Stop,
}

/// Where a visited value sits inside its parent.
#[derive(Clone, Copy)]
pub enum VisitSlot<'a> {
    /// The value the walk started with.
    Root,
    /// A field of a struct, tuple struct, tuple or enum variant.
    Field {
        /// The name; `None` for unnamed fields.
        name: Option<&'static str>,
        /// The position among the reflected fields.
        index: usize,
        /// The description of the field, if the parent has one (a dynamic value with no represented type
        /// has none).
        info: Option<&'static FieldInfo>,
    },
    /// An item of a list, array or set.
    Item(usize),
    /// The key of the map entry at the given position.
    MapKey(usize),
    /// The value of the map entry at the given position.
    MapValue {
        /// The position of the entry.
        index: usize,
        /// The key of the entry.
        key: &'a dyn Reflect,
    },
}

impl fmt::Debug for VisitSlot<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Root => f.write_str("Root"),
            Self::Field { name, index, .. } => f
                .debug_struct("Field")
                .field("name", name)
                .field("index", index)
                .finish_non_exhaustive(),
            Self::Item(index) => f.debug_tuple("Item").field(index).finish(),
            Self::MapKey(index) => f.debug_tuple("MapKey").field(index).finish(),
            Self::MapValue { index, .. } => f
                .debug_struct("MapValue")
                .field("index", index)
                .finish_non_exhaustive(),
        }
    }
}

/// Where the walker is: given to every [`ReflectVisitor`] call.
#[derive(Clone, Copy, Debug)]
pub struct VisitNode<'a> {
    path: &'a str,
    depth: usize,
    slot: VisitSlot<'a>,
}

impl<'a> VisitNode<'a> {
    /// The path of the value from the root, in the text form of [`ReflectPath`](crate::ReflectPath)
    /// (`""` for the root, `stats.health`, `items[2].name`). Map keys and values and set items have no
    /// `ReflectPath` form; they read `[key 3]`, `[value 3]` and `[item 3]` (the number is the position in
    /// iteration order). Meant for messages and diagnostics.
    pub fn path(&self) -> &'a str {
        self.path
    }

    /// How deep the value is: 0 for the root.
    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Where the value sits inside its parent.
    pub fn slot(&self) -> VisitSlot<'a> {
        self.slot
    }

    /// The description of the field the value is in, when it is a field and the parent knows it.
    pub fn field(&self) -> Option<&'static FieldInfo> {
        match self.slot {
            VisitSlot::Field { info, .. } => info,
            _ => None,
        }
    }

    /// The metadata of the field the value is in; `None` when it is not a described field.
    pub fn metadata(&self) -> Option<&'static Metadata> {
        self.field().map(FieldInfo::metadata)
    }
}

/// A visitor for [`visit`]. Both methods have do-nothing defaults.
///
/// Errors: the visitor picks its own error type; the first `Err` ends the walk and is returned by [`visit`].
pub trait ReflectVisitor {
    /// The error a visit can fail with (use [`std::convert::Infallible`] for none).
    type Error;

    /// Called when the walk reaches a value, before its children.
    ///
    /// # Errors
    ///
    /// Whatever the visitor decides; it ends the walk.
    fn enter(
        &mut self,
        node: &VisitNode<'_>,
        value: &dyn Reflect,
    ) -> Result<VisitControl, Self::Error> {
        let _ = (node, value);
        Ok(VisitControl::Continue)
    }

    /// Called after the children of a value (or right after `enter` when the children were skipped or there
    /// are none).
    ///
    /// # Errors
    ///
    /// Whatever the visitor decides; it ends the walk.
    fn leave(&mut self, node: &VisitNode<'_>, value: &dyn Reflect) -> Result<(), Self::Error> {
        let _ = (node, value);
        Ok(())
    }
}

/// Walk `value` depth-first and call `visitor` for every value (see the module documentation for the order).
///
/// Struct fields and tuple items come in declaration order, enums show the fields of the active variant only,
/// maps show each key and then its value, in the iteration order of the map.
///
/// # Errors
///
/// The first error the visitor returns.
pub fn visit<V: ReflectVisitor + ?Sized>(
    value: &dyn Reflect,
    visitor: &mut V,
) -> Result<(), V::Error> {
    let mut walker = Walker {
        visitor,
        path: String::new(),
        depth: 0,
    };
    walker.node(value, VisitSlot::Root)?;
    Ok(())
}

/// Whether the walk goes on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

struct Walker<'v, V: ReflectVisitor + ?Sized> {
    visitor: &'v mut V,
    path: String,
    depth: usize,
}

/// `?`, plus an early return when the visitor asked to stop.
macro_rules! go {
    ($call:expr) => {
        if $call? == Flow::Stop {
            return Ok(Flow::Stop);
        }
    };
}

impl<V: ReflectVisitor + ?Sized> Walker<'_, V> {
    fn node(&mut self, value: &dyn Reflect, slot: VisitSlot<'_>) -> Result<Flow, V::Error> {
        let control = self.visitor.enter(
            &VisitNode {
                path: &self.path,
                depth: self.depth,
                slot,
            },
            value,
        )?;
        match control {
            VisitControl::Stop => return Ok(Flow::Stop),
            VisitControl::SkipChildren => {}
            VisitControl::Continue => go!(self.children(value)),
        }
        self.visitor.leave(
            &VisitNode {
                path: &self.path,
                depth: self.depth,
                slot,
            },
            value,
        )?;
        Ok(Flow::Go)
    }

    /// Visit one child: `text` is appended to the path for the duration of the call.
    fn child(
        &mut self,
        text: &str,
        value: &dyn Reflect,
        slot: VisitSlot<'_>,
    ) -> Result<Flow, V::Error> {
        let mark = self.path.len();
        self.path.push_str(text);
        self.depth += 1;
        let flow = self.node(value, slot);
        self.depth -= 1;
        self.path.truncate(mark);
        flow
    }

    /// The path text of a field: `name` after a dot, or without it at the root.
    fn field_text(&self, field: &str) -> String {
        if self.path.is_empty() {
            field.to_owned()
        } else {
            format!(".{field}")
        }
    }

    fn children(&mut self, value: &dyn Reflect) -> Result<Flow, V::Error> {
        let represented = value.represented_type_info();
        match value.reflect_ref() {
            ReflectRef::Struct(view) => {
                let info = represented.and_then(TypeInfo::as_struct);
                for index in 0..view.field_len() {
                    let Some(item) = view.field_at(index) else {
                        continue;
                    };
                    let name = view.name_at(index);
                    let field = name.and_then(|name| info.and_then(|info| info.field(name)));
                    let text = match name {
                        Some(name) => self.field_text(name),
                        None => self.field_text(&index.to_string()),
                    };
                    go!(self.child(&text, item, VisitSlot::Field { name, index, info: field }));
                }
            }
            ReflectRef::TupleStruct(view) => {
                let info = represented.and_then(TypeInfo::as_tuple_struct);
                for index in 0..view.field_len() {
                    let Some(item) = view.field(index) else {
                        continue;
                    };
                    let field = info.and_then(|info| info.field_at(index));
                    let text = self.field_text(&index.to_string());
                    go!(self.child(
                        &text,
                        item,
                        VisitSlot::Field {
                            name: None,
                            index,
                            info: field
                        }
                    ));
                }
            }
            ReflectRef::Tuple(view) => {
                let info = represented.and_then(TypeInfo::as_tuple);
                for index in 0..view.field_len() {
                    let Some(item) = view.field(index) else {
                        continue;
                    };
                    let field = info.and_then(|info| info.field_at(index));
                    let text = self.field_text(&index.to_string());
                    go!(self.child(
                        &text,
                        item,
                        VisitSlot::Field {
                            name: None,
                            index,
                            info: field
                        }
                    ));
                }
            }
            ReflectRef::Enum(view) => {
                let variant = represented
                    .and_then(TypeInfo::as_enum)
                    .and_then(|info| info.variant(view.variant_name()));
                for index in 0..view.field_len() {
                    let Some(item) = view.field_at(index) else {
                        continue;
                    };
                    let name = view.name_at(index);
                    let field = variant.and_then(|variant| match name {
                        Some(name) => variant.field(name),
                        None => variant.field_at(index),
                    });
                    let text = match name {
                        Some(name) => self.field_text(name),
                        None => self.field_text(&index.to_string()),
                    };
                    go!(self.child(&text, item, VisitSlot::Field { name, index, info: field }));
                }
            }
            ReflectRef::List(view) => {
                for (index, item) in view.iter().enumerate() {
                    go!(self.child(&format!("[{index}]"), item, VisitSlot::Item(index)));
                }
            }
            ReflectRef::Array(view) => {
                for (index, item) in view.iter().enumerate() {
                    go!(self.child(&format!("[{index}]"), item, VisitSlot::Item(index)));
                }
            }
            ReflectRef::Set(view) => {
                for (index, item) in view.iter().enumerate() {
                    go!(self.child(&format!("[item {index}]"), item, VisitSlot::Item(index)));
                }
            }
            ReflectRef::Map(view) => {
                for (index, (key, item)) in view.iter().enumerate() {
                    go!(self.child(&format!("[key {index}]"), key, VisitSlot::MapKey(index)));
                    go!(self.child(
                        &format!("[value {index}]"),
                        item,
                        VisitSlot::MapValue { index, key }
                    ));
                }
            }
            // Reflection does not look into opaque values: no children.
            ReflectRef::Opaque(_) => {}
        }
        Ok(Flow::Go)
    }
}
