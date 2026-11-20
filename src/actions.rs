//! 键盘动作。绑定在 `main` 里按上下文区分：曲库快捷键不会吃掉输入框里的按键。

use gpui::actions;

actions!(
    tfy,
    [
        PlayPause,
        SeekBack,
        SeekForward,
        VolumeUp,
        VolumeDown,
        Mute,
        FocusSearch,
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
