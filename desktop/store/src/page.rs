//! Where the person is, and what the running job is for.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Page {
    New,
    Task,
    #[default]
    Library,
    Settings,
    Result,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Convert,
    Models,
}
