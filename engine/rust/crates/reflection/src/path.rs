//! Paths into reflected values: `stats.health`, `items[2].name`, `pos.1`, `boss.0.0.name`.
//!
//! * `.name` picks a field of a struct (or of a struct variant of the current enum value) by name,
//! * `.0` picks a field of a tuple, a tuple struct or a tuple variant by position (so `Option<T>` is
//!   `value.0` and a `Box<T>` is `value.0` too),
//! * `[2]` picks an item of a list or an array.
//!
//! Map entries have no path syntax. The first segment may leave out its dot; the empty path is the value
//! itself.

use std::fmt;

use crate::error::ReflectError;
use crate::reflect::{Reflect, ReflectMut, ReflectRef, Typed};

/// One step of a [`ReflectPath`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathSegment {
    /// `.name`: a field by name.
    Field(String),
    /// `.0`: a field by position.
    TupleField(usize),
    /// `[0]`: an item of a list or an array.
    Index(usize),
}

impl fmt::Display for PathSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Field(name) => write!(f, ".{name}"),
            Self::TupleField(index) => write!(f, ".{index}"),
            Self::Index(index) => write!(f, "[{index}]"),
        }
    }
}

/// A parsed path into a reflected value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectPath {
    text: String,
    segments: Vec<PathSegment>,
}

impl ReflectPath {
    /// Parses a path.
    ///
    /// # Errors
    ///
    /// [`ReflectError::InvalidPath`] with the byte position of the problem.
    pub fn parse(text: &str) -> Result<Self, ReflectError> {
        let invalid = |position: usize, reason: &'static str| ReflectError::InvalidPath {
            path: text.to_string(),
            position,
            reason,
        };
        let mut segments = Vec::new();
        let mut position = 0;
        while position < text.len() {
            let rest = &text[position..];
            if rest.starts_with('[') {
                let close = rest
                    .find(']')
                    .ok_or_else(|| invalid(position, "missing `]`"))?;
                let digits = &rest[1..close];
                if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(invalid(position + 1, "expected an index"));
                }
                let index = digits
                    .parse::<usize>()
                    .map_err(|_| invalid(position + 1, "the index is too large"))?;
                segments.push(PathSegment::Index(index));
                position += close + 1;
            } else {
                let dotted = rest.starts_with('.');
                if !dotted && !segments.is_empty() {
                    return Err(invalid(position, "expected `.` or `[`"));
                }
                let start = usize::from(dotted);
                let end = rest[start..]
                    .find(['.', '[', ']'])
                    .map_or(rest.len(), |offset| start + offset);
                let name = &rest[start..end];
                if name.is_empty() {
                    return Err(invalid(
                        position + start,
                        "expected a field name or an index",
                    ));
                }
                if name.bytes().all(|byte| byte.is_ascii_digit()) {
                    let index = name
                        .parse::<usize>()
                        .map_err(|_| invalid(position + start, "the index is too large"))?;
                    segments.push(PathSegment::TupleField(index));
                } else {
                    segments.push(PathSegment::Field(name.to_string()));
                }
                position += end;
            }
        }
        Ok(Self {
            text: text.to_string(),
            segments,
        })
    }

    /// The text the path was parsed from.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The segments, first to last.
    pub fn segments(&self) -> &[PathSegment] {
        &self.segments
    }

    fn mismatch(&self, segment: usize, reason: &'static str) -> ReflectError {
        ReflectError::PathMismatch {
            path: self.text.clone(),
            segment,
            reason,
        }
    }

    /// The value the path leads to, starting from `root`.
    ///
    /// # Errors
    ///
    /// [`ReflectError::PathMismatch`] if a segment does not fit: a missing field, an index out of bounds, a
    /// segment that does not suit the kind of the value.
    pub fn reflect<'a>(&self, root: &'a dyn Reflect) -> Result<&'a dyn Reflect, ReflectError> {
        let mut current = root;
        for (index, segment) in self.segments.iter().enumerate() {
            current = step(current, segment).map_err(|reason| self.mismatch(index, reason))?;
        }
        Ok(current)
    }

    /// Like [`ReflectPath::reflect`], for changing.
    ///
    /// # Errors
    ///
    /// The errors of [`ReflectPath::reflect`].
    pub fn reflect_mut<'a>(
        &self,
        root: &'a mut dyn Reflect,
    ) -> Result<&'a mut dyn Reflect, ReflectError> {
        let mut current = root;
        for (index, segment) in self.segments.iter().enumerate() {
            current = step_mut(current, segment).map_err(|reason| self.mismatch(index, reason))?;
        }
        Ok(current)
    }
}

impl fmt::Display for ReflectPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

const NO_FIELD: &str = "no field with that name";
const NO_INDEX: &str = "no field at that index";
const OUT_OF_BOUNDS: &str = "index out of bounds";
const WRONG_KIND: &str = "the segment does not fit the kind of the value";

fn step<'a>(
    value: &'a dyn Reflect,
    segment: &PathSegment,
) -> Result<&'a dyn Reflect, &'static str> {
    match (value.reflect_ref(), segment) {
        (ReflectRef::Struct(view), PathSegment::Field(name)) => view.field(name).ok_or(NO_FIELD),
        (ReflectRef::Enum(view), PathSegment::Field(name)) => view.field(name).ok_or(NO_FIELD),
        (ReflectRef::TupleStruct(view), PathSegment::TupleField(index)) => {
            view.field(*index).ok_or(NO_INDEX)
        }
        (ReflectRef::Tuple(view), PathSegment::TupleField(index)) => {
            view.field(*index).ok_or(NO_INDEX)
        }
        (ReflectRef::Enum(view), PathSegment::TupleField(index)) => {
            view.field_at(*index).ok_or(NO_INDEX)
        }
        (ReflectRef::List(view), PathSegment::Index(index)) => {
            view.get(*index).ok_or(OUT_OF_BOUNDS)
        }
        (ReflectRef::Array(view), PathSegment::Index(index)) => {
            view.get(*index).ok_or(OUT_OF_BOUNDS)
        }
        _ => Err(WRONG_KIND),
    }
}

fn step_mut<'a>(
    value: &'a mut dyn Reflect,
    segment: &PathSegment,
) -> Result<&'a mut dyn Reflect, &'static str> {
    match (value.reflect_mut(), segment) {
        (ReflectMut::Struct(view), PathSegment::Field(name)) => {
            view.field_mut(name).ok_or(NO_FIELD)
        }
        (ReflectMut::Enum(view), PathSegment::Field(name)) => view.field_mut(name).ok_or(NO_FIELD),
        (ReflectMut::TupleStruct(view), PathSegment::TupleField(index)) => {
            view.field_mut(*index).ok_or(NO_INDEX)
        }
        (ReflectMut::Tuple(view), PathSegment::TupleField(index)) => {
            view.field_mut(*index).ok_or(NO_INDEX)
        }
        (ReflectMut::Enum(view), PathSegment::TupleField(index)) => {
            view.field_at_mut(*index).ok_or(NO_INDEX)
        }
        (ReflectMut::List(view), PathSegment::Index(index)) => {
            view.get_mut(*index).ok_or(OUT_OF_BOUNDS)
        }
        (ReflectMut::Array(view), PathSegment::Index(index)) => {
            view.get_mut(*index).ok_or(OUT_OF_BOUNDS)
        }
        _ => Err(WRONG_KIND),
    }
}

/// Path access for every reflected value, concrete or `dyn Reflect`; implemented for all of them.
pub trait GetPath: Reflect {
    /// The value at `path`.
    ///
    /// # Errors
    ///
    /// [`ReflectError::InvalidPath`] or [`ReflectError::PathMismatch`].
    fn reflect_path(&self, path: &str) -> Result<&dyn Reflect, ReflectError> {
        ReflectPath::parse(path)?.reflect(self.as_reflect())
    }

    /// The value at `path`, for changing.
    ///
    /// # Errors
    ///
    /// [`ReflectError::InvalidPath`] or [`ReflectError::PathMismatch`].
    fn reflect_path_mut(&mut self, path: &str) -> Result<&mut dyn Reflect, ReflectError> {
        ReflectPath::parse(path)?.reflect_mut(self.as_reflect_mut())
    }

    /// The value at `path` as a `&T`.
    ///
    /// # Errors
    ///
    /// The errors of [`GetPath::reflect_path`], and [`ReflectError::TypeMismatch`] if the value is not a `T`.
    fn get_path<T: Typed>(&self, path: &str) -> Result<&T, ReflectError> {
        let value = self.reflect_path(path)?;
        value.downcast_ref::<T>().ok_or_else(|| {
            ReflectError::type_mismatch(T::static_type_info().path(), value.type_info().path())
        })
    }

    /// The value at `path` as a `&mut T`.
    ///
    /// # Errors
    ///
    /// The errors of [`GetPath::reflect_path`], and [`ReflectError::TypeMismatch`] if the value is not a `T`.
    fn get_path_mut<T: Typed>(&mut self, path: &str) -> Result<&mut T, ReflectError> {
        let value = self.reflect_path_mut(path)?;
        let actual = value.type_info().path();
        value
            .downcast_mut::<T>()
            .ok_or_else(|| ReflectError::type_mismatch(T::static_type_info().path(), actual))
    }

    /// Replaces the value at `path` with `value`, which must have the same type.
    ///
    /// # Errors
    ///
    /// The errors of [`GetPath::reflect_path_mut`], and [`ReflectError::TypeMismatch`].
    fn set_path(&mut self, path: &str, value: Box<dyn Reflect>) -> Result<(), ReflectError> {
        self.reflect_path_mut(path)?.set(value)
    }
}

impl<T: Reflect + ?Sized> GetPath for T {}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str) -> PathSegment {
        PathSegment::Field(name.to_string())
    }

    #[test]
    fn parses_fields_indices_and_tuple_fields() {
        let path = ReflectPath::parse("stats.health").unwrap();
        assert_eq!(path.segments(), &[field("stats"), field("health")]);
        let path = ReflectPath::parse("items[2].0").unwrap();
        assert_eq!(
            path.segments(),
            &[
                field("items"),
                PathSegment::Index(2),
                PathSegment::TupleField(0)
            ]
        );
        assert_eq!(path.as_str(), "items[2].0");
        assert_eq!(path.to_string(), "items[2].0");
        let path = ReflectPath::parse(".a[0][1]").unwrap();
        assert_eq!(
            path.segments(),
            &[field("a"), PathSegment::Index(0), PathSegment::Index(1)]
        );
        assert!(ReflectPath::parse("").unwrap().segments().is_empty());
        assert_eq!(PathSegment::Field("x".to_string()).to_string(), ".x");
        assert_eq!(PathSegment::Index(3).to_string(), "[3]");
    }

    #[test]
    fn reports_where_the_text_is_wrong() {
        let cases: [(&str, usize, &str); 6] = [
            ("a..b", 2, "expected a field name or an index"),
            ("a[", 1, "missing `]`"),
            ("a[x]", 2, "expected an index"),
            ("a]b", 1, "expected `.` or `[`"),
            ("[1]x", 3, "expected `.` or `[`"),
            ("a.", 2, "expected a field name or an index"),
        ];
        for (text, position, reason) in cases {
            assert_eq!(
                ReflectPath::parse(text),
                Err(ReflectError::InvalidPath {
                    path: text.to_string(),
                    position,
                    reason
                }),
                "{text}"
            );
        }
    }
}
