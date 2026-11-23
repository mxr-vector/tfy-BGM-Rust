//! 第三方接入设置中的接口目录。

pub struct Group {
    pub title: &'static str,
    pub endpoints: &'static [(&'static str, &'static str, &'static str)],
}

pub const GROUPS: &[Group] = &[
    Group {
        title: "系统与状态",
        endpoints: &[
            ("GET", "/v1/health", "健康检查"),
            ("GET", "/v1/state", "播放状态"),
            ("GET", "/v1/events", "全局 SSE"),
        ],
    },
    Group {
        title: "播放控制",
        endpoints: &[
            ("POST", "/v1/play", "播放曲目"),
            ("POST", "/v1/pause", "暂停"),
            ("POST", "/v1/resume", "继续"),
            ("POST", "/v1/toggle", "切换播放状态"),
            ("POST", "/v1/next", "下一首"),
            ("POST", "/v1/previous", "上一首"),
            ("POST", "/v1/seek", "跳转进度"),
            ("POST", "/v1/volume", "调整音量"),
            ("POST", "/v1/crossfade", "调整淡入淡出"),
        ],
    },
    Group {
        title: "曲库",
        endpoints: &[
            ("GET", "/v1/library", "曲库快照"),
            ("POST", "/v1/library/rescan", "重新扫描曲库"),
        ],
    },
    Group {
        title: "音频输出",
        endpoints: &[
            ("GET", "/v1/outputs", "查看设备与路由"),
            ("POST", "/v1/outputs", "更新输出路由"),
        ],
    },
    Group {
        title: "字幕",
        endpoints: &[
            ("GET", "/v1/subtitles", "字幕列表"),
            ("GET", "/v1/subtitles/track", "字幕详情"),
            ("PUT", "/v1/subtitles/track", "编辑分段"),
            ("PUT", "/v1/subtitles/text", "编辑原文"),
            ("DELETE", "/v1/subtitles/track", "删除字幕"),
            ("GET", "/v1/subtitles/export", "下载字幕"),
            ("POST", "/v1/subtitles/generate", "生成字幕"),
        ],
    },
    Group {
        title: "人声分离",
        endpoints: &[
            ("GET", "/v1/separation/models", "查看分离模型"),
            ("GET", "/v1/separation/result", "查看分离结果"),
            ("POST", "/v1/separation", "提交分离任务"),
        ],
    },
    Group {
        title: "ASR 模型",
        endpoints: &[
            ("GET", "/v1/models", "查看模型状态"),
            ("POST", "/v1/models/load", "加载模型"),
            ("POST", "/v1/models/unload", "释放模型"),
        ],
    },
    Group {
        title: "后台任务",
        endpoints: &[
            ("GET", "/v1/tasks", "任务列表"),
            ("GET", "/v1/tasks/{id}", "任务详情"),
            ("GET", "/v1/tasks/{id}/events", "单任务 SSE"),
        ],
    },
    Group {
        title: "通用命令",
        endpoints: &[("POST", "/v1/command/{action}", "执行播放或输出动作")],
    },
];
