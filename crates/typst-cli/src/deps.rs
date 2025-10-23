use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{self, Write};

use crate::args::{DepsFormat, Output};
use crate::world::SystemWorld;

use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};

/// Writes dependencies in the given format.
pub fn write_deps(
    world: &mut SystemWorld,
    dest: &Output,
    format: DepsFormat,
    outputs: Option<&[Output]>,
) -> io::Result<()> {
    match format {
        DepsFormat::Json => write_deps_json(world, dest, outputs)?,
        DepsFormat::Zero => write_deps_zero(world, dest)?,
        DepsFormat::Make => {
            if let Some(outputs) = outputs {
                write_deps_make(world, dest, outputs)?;
            }
        }
    }
    Ok(())
}

/// JSON serializer for the dependencies.
///
/// Note: Serialization consumes the iterator, so this adapter cannot be reused after serialization.
/// Based on https://stackoverflow.com/a/34400370
struct InputSerializer<I: Iterator<Item = OsString>>(RefCell<I>);

impl<I: Iterator<Item = OsString>> InputSerializer<I> {
    fn new(iterator: I) -> Self {
        Self(RefCell::new(iterator))
    }
}

impl<I: Iterator<Item = OsString>> Serialize for InputSerializer<I> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        // Note that the iterator is consumed here:
        for dep in self.0.borrow_mut().by_ref() {
            let s = dep.to_str().ok_or_else(|| {
                serde::ser::Error::custom(format!("input {dep:?} is not valid utf-8"))
            })?;
            seq.serialize_element(s)?;
        }
        seq.end()
    }
}

/// JSON Serializer for the outputs.
struct OutputSerializer<'a>(&'a [Output]);

impl Serialize for OutputSerializer<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(None)?;
        for output in self.0 {
            match output {
                Output::Path(path) => {
                    let s = path.as_os_str().to_str().ok_or_else(|| {
                        <S::Error as serde::ser::Error>::custom(format!(
                            "output {path:?} is not valid utf-8"
                        ))
                    })?;
                    seq.serialize_element(s)?;
                }
                Output::Stdout => {} // Skip stdout outputs.
            }
        }
        seq.end()
    }
}

/// Writes dependencies in JSON format.
fn write_deps_json(
    world: &mut SystemWorld,
    dest: &Output,
    outputs: Option<&[Output]>,
) -> io::Result<()> {
    let dest = dest.open()?;
    let mut serializer = serde_json::Serializer::new(dest);
    let mut map = serializer.serialize_map(Some(2))?;

    map.serialize_entry("inputs", &InputSerializer::new(relative_dependencies(world)?))?;
    match outputs {
        None => map.serialize_entry("outputs", &None::<()>)?,
        Some(outputs) => map.serialize_entry("outputs", &OutputSerializer(outputs))?,
    };

    SerializeMap::end(map)?;
    Ok(())
}

/// Writes dependencies in the Zero / Text0 format.
fn write_deps_zero(world: &mut SystemWorld, dest: &Output) -> io::Result<()> {
    let mut dest = dest.open()?;
    for dep in relative_dependencies(world)? {
        dest.write_all(dep.as_encoded_bytes())?;
        dest.write_all(b"\0")?;
    }
    Ok(())
}

/// Writes dependencies in the Make format.
fn write_deps_make(
    world: &mut SystemWorld,
    dest: &Output,
    outputs: &[Output],
) -> io::Result<()> {
    let mut dest = dest.open()?;
    for (i, output) in outputs.iter().enumerate() {
        let path = match output {
            Output::Path(path) => path.as_os_str(),
            Output::Stdout => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "make dependencies contain the output path, \
                     but the output was stdout",
                ));
            }
        };

        // Silently skip paths that aren't valid Unicode so we still
        // produce a rule that will work for the other paths that can be
        // processed.
        let Some(string) = path.to_str() else { continue };
        if i != 0 {
            dest.write_all(b" ")?;
        }
        dest.write_all(munge(string).as_bytes())?;
    }
    dest.write_all(b":")?;

    for dep in relative_dependencies(world)? {
        // See above.
        let Some(string) = dep.to_str() else { continue };
        dest.write_all(b" ")?;
        dest.write_all(munge(string).as_bytes())?;
    }
    dest.write_all(b"\n")?;

    Ok(())
}

// Based on `munge` in libcpp/mkdeps.cc from the GCC source code. This isn't
// perfect as some special characters can't be escaped.
fn munge(s: &str) -> String {
    let mut res = String::with_capacity(s.len());
    let mut slashes = 0;
    for c in s.chars() {
        match c {
            '\\' => slashes += 1,
            '$' => {
                res.push('$');
                slashes = 0;
            }
            ':' => {
                res.push('\\');
                slashes = 0;
            }
            ' ' | '\t' => {
                // `munge`'s source contains a comment here that says: "A
                // space or tab preceded by 2N+1 backslashes represents N
                // backslashes followed by space..."
                for _ in 0..slashes + 1 {
                    res.push('\\');
                }
                slashes = 0;
            }
            '#' => {
                res.push('\\');
                slashes = 0;
            }
            _ => slashes = 0,
        };
        res.push(c);
    }
    res
}

/// Extracts the current compilation's dependencies as paths relative to the
/// current directory.
fn relative_dependencies(
    world: &mut SystemWorld,
) -> io::Result<impl Iterator<Item = OsString>> {
    let root = world.root().to_owned();
    let current_dir = std::env::current_dir()?;
    let relative_root =
        pathdiff::diff_paths(&root, &current_dir).unwrap_or_else(|| root.clone());
    Ok(world.dependencies().map(move |dependency| {
        dependency
            .strip_prefix(&root)
            .map_or_else(|_| dependency.clone(), |x| relative_root.join(x))
            .into_os_string()
    }))
}
