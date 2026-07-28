use anyhow::{Context, Result};
use std::{fs, path::Path};

pub(crate) fn read_source(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("read source {}", path.display()))?;
    Ok(sanitize_utf8(bytes))
}

fn sanitize_utf8(bytes: Vec<u8>) -> String {
    if String::from_utf8(bytes.clone()).is_ok() {
        return String::from_utf8(bytes).expect("validated UTF-8");
    }
    let mut output = Vec::with_capacity(bytes.len());
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(_) => {
                output.extend_from_slice(remaining);
                break;
            }
            Err(error) => {
                let valid = error.valid_up_to();
                output.extend_from_slice(&remaining[..valid]);
                remaining = &remaining[valid..];
                let invalid = error.error_len().unwrap_or(remaining.len());
                for byte in &remaining[..invalid] {
                    output.push(if matches!(*byte, b'\n' | b'\r') {
                        *byte
                    } else {
                        b' '
                    });
                }
                remaining = &remaining[invalid..];
            }
        }
    }
    String::from_utf8(output).expect("invalid UTF-8 bytes are replaced byte-for-byte")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_utf8_is_replaced_without_moving_source_offsets() {
        let source = sanitize_utf8(b"fn before() {}\n// bad: \xff\nfn after() {}\n".to_vec());
        assert_eq!(source.len(), 39);
        assert_eq!(source.lines().count(), 3);
        assert_eq!(source.find("fn after"), Some(25));
        assert!(source.contains("// bad:  "));
    }
}
