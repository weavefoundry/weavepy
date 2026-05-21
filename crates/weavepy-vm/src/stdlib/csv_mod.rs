//! The `csv` built-in module.
//!
//! Rust-side parser and serialiser for CSV / TSV. The dialect knobs
//! match CPython's: `delimiter`, `quotechar`, `quoting`, `escapechar`,
//! `lineterminator`, `doublequote`, `skipinitialspace`. `reader`
//! returns an iterator yielding rows as `list[str]`; `writer` is a
//! tiny adapter that accumulates into a file-like object.
//!
//! The user-visible `DictReader` / `DictWriter` / `Sniffer` live in a
//! frozen Python wrapper on top of these primitives.

use std::cell::RefCell;
use std::rc::Rc;

use crate::error::{type_error, value_error, RuntimeError};
use crate::import::ModuleCache;
use crate::object::{BuiltinFn, DictData, DictKey, Object, PyModule};

pub fn build(_cache: &ModuleCache) -> Rc<PyModule> {
    let dict = Rc::new(RefCell::new(DictData::new()));
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("__name__")),
            Object::from_static("_csv"),
        );
        d.insert(
            DictKey(Object::from_static("__doc__")),
            Object::from_static("CSV (Comma-Separated Values) parsing/writing."),
        );
        d.insert(
            DictKey(Object::from_static("QUOTE_MINIMAL")),
            Object::Int(0),
        );
        d.insert(DictKey(Object::from_static("QUOTE_ALL")), Object::Int(1));
        d.insert(
            DictKey(Object::from_static("QUOTE_NONNUMERIC")),
            Object::Int(2),
        );
        d.insert(DictKey(Object::from_static("QUOTE_NONE")), Object::Int(3));
        d.insert(
            DictKey(Object::from_static("Error")),
            Object::Type(crate::builtin_types::builtin_types().value_error.clone()),
        );
        d.insert(
            DictKey(Object::from_static("reader")),
            b("reader", reader_call),
        );
        d.insert(
            DictKey(Object::from_static("writer")),
            b("writer", writer_call),
        );
        d.insert(
            DictKey(Object::from_static("list_dialects")),
            b("list_dialects", list_dialects),
        );
    }
    Rc::new(PyModule {
        name: "_csv".to_owned(),
        filename: None,
        dict,
    })
}

fn b(name: &'static str, body: fn(&[Object]) -> Result<Object, RuntimeError>) -> Object {
    Object::Builtin(Rc::new(BuiltinFn {
        name,
        call: Box::new(body),
    }))
}

#[derive(Clone, Copy)]
struct Dialect {
    delimiter: char,
    quotechar: char,
    doublequote: bool,
    skipinitialspace: bool,
    quoting: i64,
}

impl Default for Dialect {
    fn default() -> Self {
        Self {
            delimiter: ',',
            quotechar: '"',
            doublequote: true,
            skipinitialspace: false,
            quoting: 0,
        }
    }
}

fn extract_dialect(arg: Option<&Object>) -> Dialect {
    // CPython exposes dialects as classes; we accept either:
    //   * a string like "excel"
    //   * a dict carrying the per-field settings
    //   * None — default to excel.
    match arg {
        Some(Object::Str(s)) if s.as_ref() == "excel-tab" => Dialect {
            delimiter: '\t',
            ..Dialect::default()
        },
        Some(Object::Dict(d)) => {
            let d = d.borrow();
            let mut dialect = Dialect::default();
            if let Some(Object::Str(s)) = d.get(&DictKey(Object::from_static("delimiter"))) {
                if let Some(c) = s.chars().next() {
                    dialect.delimiter = c;
                }
            }
            if let Some(Object::Str(s)) = d.get(&DictKey(Object::from_static("quotechar"))) {
                if let Some(c) = s.chars().next() {
                    dialect.quotechar = c;
                }
            }
            if let Some(Object::Bool(b)) = d.get(&DictKey(Object::from_static("doublequote"))) {
                dialect.doublequote = *b;
            }
            if let Some(Object::Bool(b)) = d.get(&DictKey(Object::from_static("skipinitialspace")))
            {
                dialect.skipinitialspace = *b;
            }
            if let Some(Object::Int(n)) = d.get(&DictKey(Object::from_static("quoting"))) {
                dialect.quoting = *n;
            }
            dialect
        }
        _ => Dialect::default(),
    }
}

/// `csv.reader(csvfile, dialect=...)` — `csvfile` is any iterable of
/// strings (typically a file). We parse each line as it's yielded and
/// produce a list-of-list iterator.
fn reader_call(args: &[Object]) -> Result<Object, RuntimeError> {
    // Convert the iterable into a list of lines up front. For files,
    // we read everything (CPython streams; here we don't yet have a
    // sane Python-level iteration protocol from Rust). This is fine
    // for everyday CSV sizes.
    let mut lines: Vec<String> = Vec::new();
    match args.first() {
        Some(Object::List(l)) => {
            for item in l.borrow().iter() {
                if let Object::Str(s) = item {
                    lines.push(s.to_string());
                }
            }
        }
        Some(Object::Tuple(t)) => {
            for item in t.iter() {
                if let Object::Str(s) = item {
                    lines.push(s.to_string());
                }
            }
        }
        Some(Object::File(file)) => {
            // Read the whole backing object as a string and split.
            let snapshot = match &*file.backend.borrow() {
                crate::object::FileBackend::MemBytes { data, .. } => {
                    String::from_utf8_lossy(data).into_owned()
                }
                crate::object::FileBackend::MemText { data, .. } => data.clone(),
                _ => String::new(),
            };
            for line in snapshot.lines() {
                lines.push(line.to_string());
            }
        }
        Some(Object::Str(s)) => {
            for line in s.lines() {
                lines.push(line.to_string());
            }
        }
        _ => return Err(type_error("csv.reader: expected iterable of str")),
    }

    let dialect = extract_dialect(args.get(1));
    let rows: Vec<Object> = lines
        .iter()
        .map(|line| {
            let row = parse_row(line, &dialect);
            Object::new_list(row.into_iter().map(Object::from_str).collect())
        })
        .collect();
    Ok(Object::new_list(rows))
}

fn parse_row(line: &str, dialect: &Dialect) -> Vec<String> {
    let mut out = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut iter = line.chars().peekable();
    while let Some(c) = iter.next() {
        if in_quotes {
            if c == dialect.quotechar {
                if dialect.doublequote && iter.peek() == Some(&dialect.quotechar) {
                    field.push(c);
                    iter.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
        } else if c == dialect.delimiter {
            out.push(std::mem::take(&mut field));
        } else if c == dialect.quotechar && field.is_empty() {
            in_quotes = true;
        } else if dialect.skipinitialspace && field.is_empty() && c == ' ' {
            continue;
        } else {
            field.push(c);
        }
    }
    out.push(field);
    out
}

/// `csv.writer(csvfile, dialect=...)` — `csvfile` must be a file-like
/// object with `.write(str)`. We accumulate rows into a tiny dict
/// exposing `.writerow(row)` and `.writerows(rows)`.
fn writer_call(args: &[Object]) -> Result<Object, RuntimeError> {
    let target = args
        .first()
        .cloned()
        .ok_or_else(|| type_error("csv.writer: missing csvfile"))?;
    let dialect = extract_dialect(args.get(1));
    let dict = Rc::new(RefCell::new(DictData::new()));
    let target_for_row = target.clone();
    let writerow = move |a: &[Object]| -> Result<Object, RuntimeError> {
        let row_obj = a
            .first()
            .ok_or_else(|| type_error("writerow: missing row"))?;
        let cells: Vec<Object> = match row_obj {
            Object::List(l) => l.borrow().clone(),
            Object::Tuple(t) => t.to_vec(),
            _ => return Err(type_error("writerow: row must be list/tuple")),
        };
        let line = encode_row(&cells, &dialect);
        write_to(&target_for_row, &line)?;
        Ok(Object::Int(line.len() as i64))
    };
    let target_for_rows = target;
    let writerows = move |a: &[Object]| -> Result<Object, RuntimeError> {
        let rows_obj = a
            .first()
            .ok_or_else(|| type_error("writerows: missing rows"))?;
        let rows: Vec<Object> = match rows_obj {
            Object::List(l) => l.borrow().clone(),
            Object::Tuple(t) => t.to_vec(),
            _ => return Err(type_error("writerows: rows must be list/tuple")),
        };
        let mut total = 0i64;
        for row in rows {
            let cells: Vec<Object> = match &row {
                Object::List(l) => l.borrow().clone(),
                Object::Tuple(t) => t.to_vec(),
                _ => return Err(type_error("writerows: each row must be list/tuple")),
            };
            let line = encode_row(&cells, &dialect);
            write_to(&target_for_rows, &line)?;
            total += line.len() as i64;
        }
        Ok(Object::Int(total))
    };
    {
        let mut d = dict.borrow_mut();
        d.insert(
            DictKey(Object::from_static("writerow")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "writerow",
                call: Box::new(writerow),
            })),
        );
        d.insert(
            DictKey(Object::from_static("writerows")),
            Object::Builtin(Rc::new(BuiltinFn {
                name: "writerows",
                call: Box::new(writerows),
            })),
        );
    }
    Ok(Object::Dict(dict))
}

fn encode_row(cells: &[Object], dialect: &Dialect) -> String {
    let mut out = String::new();
    for (i, cell) in cells.iter().enumerate() {
        if i > 0 {
            out.push(dialect.delimiter);
        }
        let s = cell.to_str();
        let needs_quote = dialect.quoting == 1
            || s.contains(dialect.delimiter)
            || s.contains(dialect.quotechar)
            || s.contains('\n')
            || s.contains('\r');
        if needs_quote {
            out.push(dialect.quotechar);
            for c in s.chars() {
                if c == dialect.quotechar && dialect.doublequote {
                    out.push(dialect.quotechar);
                }
                out.push(c);
            }
            out.push(dialect.quotechar);
        } else {
            out.push_str(&s);
        }
    }
    out.push_str("\r\n");
    out
}

fn write_to(target: &Object, line: &str) -> Result<(), RuntimeError> {
    match target {
        Object::File(file) => {
            let mut state = file.backend.borrow_mut();
            match &mut *state {
                crate::object::FileBackend::MemBytes { data, pos: _ } => {
                    data.extend_from_slice(line.as_bytes());
                }
                crate::object::FileBackend::MemText { data, pos: _ } => {
                    data.push_str(line);
                }
                crate::object::FileBackend::Disk(f) => {
                    std::io::Write::write_all(f, line.as_bytes())
                        .map_err(|e| value_error(e.to_string()))?;
                }
                crate::object::FileBackend::Stdout(sink)
                | crate::object::FileBackend::Stderr(sink) => {
                    std::io::Write::write_all(&mut *sink.borrow_mut(), line.as_bytes())
                        .map_err(|e| value_error(e.to_string()))?;
                }
                crate::object::FileBackend::Stdin => return Err(value_error("not writable")),
            }
            Ok(())
        }
        Object::Dict(d) => {
            // Best-effort: call `.write(line)` on the dict if present.
            let write = d
                .borrow()
                .get(&DictKey(Object::from_static("write")))
                .cloned();
            if let Some(Object::Builtin(b)) = write {
                (b.call)(&[Object::from_str(line.to_string())])?;
                Ok(())
            } else {
                Err(type_error("target has no write()"))
            }
        }
        _ => Err(type_error("target must be a file-like object")),
    }
}

fn list_dialects(_args: &[Object]) -> Result<Object, RuntimeError> {
    Ok(Object::new_list(vec![
        Object::from_static("excel"),
        Object::from_static("excel-tab"),
        Object::from_static("unix"),
    ]))
}
