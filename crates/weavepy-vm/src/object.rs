//! The runtime object model.
//!
//! This is an early sketch. The eventual representation will use a tagged or
//! NaN-boxed pointer to allow uniform handling of all Python objects with low
//! overhead for small integers and other immortal singletons.

/// A Python value as seen by the interpreter.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    None,
    Bool(bool),
    Int(i128),
    Float(f64),
    Str(String),
}

impl Object {
    /// CPython's notion of truthiness for the small set of values we model so
    /// far. Will be replaced by a dispatch on the object's type once we have
    /// type objects with `__bool__` / `__len__` slots.
    pub fn is_truthy(&self) -> bool {
        match self {
            Object::None => false,
            Object::Bool(b) => *b,
            Object::Int(i) => *i != 0,
            Object::Float(f) => *f != 0.0 && !f.is_nan(),
            Object::Str(s) => !s.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_matches_python_basics() {
        assert!(!Object::None.is_truthy());
        assert!(!Object::Bool(false).is_truthy());
        assert!(Object::Bool(true).is_truthy());
        assert!(!Object::Int(0).is_truthy());
        assert!(Object::Int(1).is_truthy());
        assert!(!Object::Float(0.0).is_truthy());
        assert!(!Object::Float(f64::NAN).is_truthy());
        assert!(Object::Float(1.5).is_truthy());
        assert!(!Object::Str(String::new()).is_truthy());
        assert!(Object::Str("x".into()).is_truthy());
    }
}
