//! Backend names and GPU choices shared by native and browser configuration.

use std::fmt;

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

/// Which GPUs mine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum DeviceSelection {
    /// Every discrete GPU; integrated GPUs only when no discrete GPU exists.
    #[default]
    Default,
    /// Every GPU, integrated GPUs included.
    WithIntegrated,
    /// These GPUs, by the numbers `devices` prints.
    Indices(Vec<u32>),
}

impl DeviceSelection {
    /// Parses `--device`: `all`, one number, or a comma-separated list.
    pub fn parse(value: &str) -> Result<Self, String> {
        let value = value.trim();
        if value.eq_ignore_ascii_case("all") {
            return Ok(Self::Default);
        }
        let mut indices = Vec::new();
        for item in value.split(',') {
            let index = item.trim().parse::<u32>().map_err(|_| {
                format!("--device takes all, a GPU number or a list like 0,2; not {value:?}")
            })?;
            if indices.contains(&index) {
                return Err(format!("--device lists GPU {index} twice"));
            }
            indices.push(index);
        }
        Ok(Self::Indices(indices))
    }

    /// Adds integrated GPUs to the default choice; an explicit list stays as given.
    pub fn with_integrated(self, include: bool) -> Self {
        match self {
            Self::Default if include => Self::WithIntegrated,
            other => other,
        }
    }

    /// The single GPU number for commands that run one GPU at a time.
    pub fn single_index(&self) -> Result<Option<u32>, String> {
        match self {
            Self::Indices(indices) if indices.len() > 1 => {
                Err("this command runs one GPU at a time; choose one with --device N".into())
            }
            Self::Indices(indices) => Ok(indices.first().copied()),
            Self::Default | Self::WithIntegrated => Ok(None),
        }
    }
}

impl fmt::Display for DeviceSelection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => formatter.write_str("all"),
            Self::WithIntegrated => formatter.write_str("all, integrated included"),
            Self::Indices(indices) => {
                let list: Vec<String> = indices.iter().map(u32::to_string).collect();
                formatter.write_str(&list.join(","))
            }
        }
    }
}
