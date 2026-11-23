//! 曲库、对话框和输入框的键盘动作。

use gpui::actions;

actions!(
    tfy,
    [
        PlayPause,
        TrackPage,
        ToggleDesktopLyrics,
        SeekBack,
        SeekForward,
        VolumeUp,
        VolumeDown,
        Mute,
        FocusSearch,
        OpenSettings,
        Dismiss,
        Confirm,
        Quit,
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
    ]
);
