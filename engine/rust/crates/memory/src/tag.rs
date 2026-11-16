//! The categories memory is accounted under.

use core::fmt;

/// The engine system a block of memory belongs to. Tagged allocations (see [`Bump::with_capacity_tagged`]) are
/// counted per tag by the [`MemoryTracker`], so a report can say how much memory rendering, assets or the ECS use,
/// and a budget can be set per system.
///
/// When a variant is added, [`MemoryTag::COUNT`] and [`MemoryTag::ALL`] must grow with it; a unit test checks that
/// every variant of `ALL` has a distinct index below `COUNT`.
///
/// [`Bump::with_capacity_tagged`]: crate::Bump::with_capacity_tagged
/// [`MemoryTracker`]: crate::MemoryTracker
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum MemoryTag {
    /// Memory that belongs to no particular system (the default).
    #[default]
    General,
    /// The renderer: buffers, command lists, frame data.
    Rendering,
    /// Assets and resources: textures, meshes, sounds.
    Assets,
    /// The entity-component system.
    Ecs,
    /// Physics.
    Physics,
    /// Audio.
    Audio,
    /// Scripting.
    Scripting,
    /// User interface.
    Ui,
    /// Networking.
    Networking,
    /// Editor and tools.
    Tools,
}

impl MemoryTag {
    /// The number of tags.
    pub const COUNT: usize = 10;

    /// Every tag, in index order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::General,
        Self::Rendering,
        Self::Assets,
        Self::Ecs,
        Self::Physics,
        Self::Audio,
        Self::Scripting,
        Self::Ui,
        Self::Networking,
        Self::Tools,
    ];

    /// The position of the tag in [`MemoryTag::ALL`], below [`MemoryTag::COUNT`].
    pub const fn index(self) -> usize {
        self as usize
    }

    /// The name of the tag, for reports.
    pub const fn name(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Rendering => "Rendering",
            Self::Assets => "Assets",
            Self::Ecs => "Ecs",
            Self::Physics => "Physics",
            Self::Audio => "Audio",
            Self::Scripting => "Scripting",
            Self::Ui => "Ui",
            Self::Networking => "Networking",
            Self::Tools => "Tools",
        }
    }
}

impl fmt::Display for MemoryTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tag_has_a_distinct_index_below_count() {
        for (position, tag) in MemoryTag::ALL.iter().enumerate() {
            assert_eq!(tag.index(), position, "{tag}");
        }
        assert_eq!(MemoryTag::Tools.index() + 1, MemoryTag::COUNT);
    }

    #[test]
    fn names_are_distinct_and_printed() {
        let mut names: Vec<&str> = MemoryTag::ALL.iter().map(|tag| tag.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), MemoryTag::COUNT);
        assert_eq!(MemoryTag::Rendering.to_string(), "Rendering");
    }

    #[test]
    fn the_default_tag_is_general() {
        assert_eq!(MemoryTag::default(), MemoryTag::General);
    }
}
