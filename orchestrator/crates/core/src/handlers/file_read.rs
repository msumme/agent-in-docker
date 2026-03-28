use std::path::Path;

/// Read a file from the host filesystem.
/// Called after permission checks and human approval.
pub fn read_file(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    if !p.exists() {
        return Err(format!("File not found: {}", path));
    }
    if !p.is_file() {
        return Err(format!("Not a file: {}", path));
    }
    std::fs::read_to_string(p).map_err(|e| format!("Failed to read '{}': {}", path, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("test.txt");
        let mut f = std::fs::File::create(&file_path).unwrap();
        write!(f, "hello world").unwrap();

        let result = read_file(file_path.to_str().unwrap());
        assert_eq!(result.unwrap(), "hello world");
    }

    #[test]
    fn read_nonexistent_file() {
        let result = read_file("/nonexistent/path/file.txt");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn read_directory_fails() {
        let dir = tempfile::tempdir().unwrap();
        let result = read_file(dir.path().to_str().unwrap());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Not a file"));
    }
}
