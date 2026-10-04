//! Backend names shared by native and browser configuration.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Auto,
    Cuda,
    Hip,
    Wgpu,
}

impl BackendKind {
    /// Parses a requested GPU backend by name.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "cuda" => Ok(Self::Cuda),
            "hip" | "rocm" => Ok(Self::Hip),
            "wgpu" => Ok(Self::Wgpu),
            other => Err(format!("unknown backend `{other}` (auto|cuda|hip|wgpu)")),
        }
    }

    /// Returns the canonical CLI name of the GPU backend.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Cuda => "cuda",
            Self::Hip => "hip",
            Self::Wgpu => "wgpu",
        }
    }
}
