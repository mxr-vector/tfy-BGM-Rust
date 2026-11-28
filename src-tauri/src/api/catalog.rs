//! 开放接口目录：业务分类与接口清单。
//!
//! 界面「第三方接入」直接渲染这份数据；新增路由时同步补充 [`endpoints`]，
//! 测试会校验每条接口都属于已声明的分类。

use serde::Serialize;

/// 接口的业务分类；顺序即界面展示顺序。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCategory {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

const CAT_SYSTEM: &str = "system";
const CAT_PLAYBACK: &str = "playback";
const CAT_LIBRARY: &str = "library";
const CAT_OUTPUT: &str = "output";
const CAT_SUBTITLES: &str = "subtitles";
const CAT_SEPARATION: &str = "separation";
const CAT_MODELS: &str = "models";
const CAT_TASKS: &str = "tasks";
const CAT_COMMAND: &str = "command";

/// 当前接口的业务分类清单（固定顺序）。
pub fn categories() -> Vec<ApiCategory> {
    vec![
        ApiCategory {
            id: CAT_SYSTEM,
            label: "系统与状态",
            description: "健康检查、播放状态快照与全局事件流",
        },
        ApiCategory {
            id: CAT_PLAYBACK,
            label: "播放控制",
            description: "播放、切歌、进度、音量与淡入淡出",
        },
        ApiCategory {
            id: CAT_LIBRARY,
            label: "曲库",
            description: "曲目列表与重新扫描",
        },
        ApiCategory {
            id: CAT_OUTPUT,
            label: "音频输出",
            description: "输出设备与多路路由",
        },
        ApiCategory {
            id: CAT_SUBTITLES,
            label: "字幕",
            description: "生成任务、记录检索、编辑与导出",
        },
        ApiCategory {
            id: CAT_SEPARATION,
            label: "人声分离",
            description: "分离模型目录、独立分离任务与交付文件",
        },
        ApiCategory {
            id: CAT_MODELS,
            label: "模型",
            description: "ASR 模型清单、加载与释放",
        },
        ApiCategory {
            id: CAT_TASKS,
            label: "长任务",
            description: "串行任务队列与逐任务 SSE 进度流",
        },
        ApiCategory {
            id: CAT_COMMAND,
            label: "通用",
            description: "把任意界面动作转给前端执行",
        },
    ]
}

/// 「第三方接入」里展示的一条接口说明；与 [`router`] 注册的路由一一对应。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEndpoint {
    /// 所属业务分类 id，见 [`categories`]。
    pub category: &'static str,
    pub method: &'static str,
    pub path: &'static str,
    pub summary: &'static str,
    pub description: &'static str,
}

/// 接口目录：分类顺序 + 全部接口，界面一次读取即可分组渲染。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiCatalog {
    pub categories: Vec<ApiCategory>,
    pub endpoints: Vec<ApiEndpoint>,
}

/// 当前开放接口清单；新增路由时同步补充，界面会自动展示。
pub fn endpoints() -> Vec<ApiEndpoint> {
    vec![
        ApiEndpoint {
            category: CAT_SYSTEM,
            method: "GET",
            path: "/v1/health",
            summary: "健康检查",
            description: "返回服务版本、监听端口与就绪状态，不校验 token。",
        },
        ApiEndpoint {
            category: CAT_SYSTEM,
            method: "GET",
            path: "/v1/state",
            summary: "播放状态",
            description: "当前曲目、进度、音量、播放模式、队列与输出路由快照。",
        },
        ApiEndpoint {
            category: CAT_SYSTEM,
            method: "GET",
            path: "/v1/events",
            summary: "事件流（SSE）",
            description: "标准 SSE：event 为 state / task / resync，id 单调递增；支持 Last-Event-ID 断线续传；?legacy=1 退回无 event 的旧帧。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/play",
            summary: "播放曲目",
            description: "请求体 {\"key\":\"分类/曲名\",\"queue\":[\"分类/曲名\"]}，queue 可选。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/pause",
            summary: "暂停",
            description: "暂停当前播放；未在播放时不做任何操作。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/resume",
            summary: "继续播放",
            description: "从暂停位置继续播放。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/toggle",
            summary: "播放 / 暂停切换",
            description: "等价于界面上的播放按钮。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/next",
            summary: "下一首",
            description: "按当前播放模式切到下一首。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/previous",
            summary: "上一首",
            description: "播放超过重播阈值时先回到开头，否则切到上一首。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/seek",
            summary: "跳转进度",
            description: "请求体 {\"seconds\":12.5}，单位为秒。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/volume",
            summary: "设置主音量",
            description: "请求体 {\"volume\":0.6}，取值 0~1。",
        },
        ApiEndpoint {
            category: CAT_PLAYBACK,
            method: "POST",
            path: "/v1/crossfade",
            summary: "设置淡入淡出",
            description: "请求体 {\"seconds\":2}，取值 0~12，0 表示硬切。",
        },
        ApiEndpoint {
            category: CAT_LIBRARY,
            method: "GET",
            path: "/v1/library",
            summary: "曲库内容",
            description: "全部曲目的 key、路径、分类与音频格式，可直接用于 /v1/play。",
        },
        ApiEndpoint {
            category: CAT_LIBRARY,
            method: "POST",
            path: "/v1/library/rescan",
            summary: "重新扫描曲库",
            description: "重新读取磁盘并返回最新曲库快照。",
        },
        ApiEndpoint {
            category: CAT_OUTPUT,
            method: "GET",
            path: "/v1/outputs",
            summary: "输出设备与路由",
            description: "可用输出设备、每路路由的开关、独立音量与静音状态。",
        },
        ApiEndpoint {
            category: CAT_OUTPUT,
            method: "POST",
            path: "/v1/outputs",
            summary: "更新一路输出",
            description: "请求体 {\"deviceId\":\"...\",\"enabled\":true,\"volume\":0.8,\"muted\":false}。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "GET",
            path: "/v1/subtitles",
            summary: "字幕记录列表",
            description: "?q= 检索曲名 / 文件名 / key / 正文；默认仅元信息，include=content 才带 SRT / LRC / 分段；支持 limit / offset。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "GET",
            path: "/v1/subtitles/track",
            summary: "单条字幕记录",
            description: "?key=曲目 key；key 含 / 与中文，放在查询参数里避免路径转义。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "PUT",
            path: "/v1/subtitles/track",
            summary: "按分段更新字幕",
            description: "请求体 {\"segments\":[{\"start\":1.2,\"end\":3.4,\"text\":\"…\"}]}；校验排序后写回并同步 SRT / LRC。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "PUT",
            path: "/v1/subtitles/text",
            summary: "按原文更新字幕",
            description: "?key=&format=srt|lrc，请求体 {\"text\":\"…\"}；解析为分段后写回。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "DELETE",
            path: "/v1/subtitles/track",
            summary: "删除字幕记录",
            description: "?key=；同时清理归一化音频、人声 / 伴奏文件与临时文件。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "GET",
            path: "/v1/subtitles/export",
            summary: "导出字幕文件",
            description: "?key=&format=srt|lrc，直接返回文件内容（Content-Disposition: attachment）。",
        },
        ApiEndpoint {
            category: CAT_SUBTITLES,
            method: "POST",
            path: "/v1/subtitles/generate",
            summary: "提交字幕生成任务",
            description: "请求体 {\"key\":…,\"separate\":true|false,\"separationModel\":…,\"separationTarget\":\"vocals\"|\"instrumental\"}；separate 默认 true，false 时直接识别原始音频。返回 202 与 task。",
        },
        ApiEndpoint {
            category: CAT_SEPARATION,
            method: "GET",
            path: "/v1/separation/models",
            summary: "分离模型目录",
            description: "可选人声模型与固定伴奏模型（UVR-MDX-NET-Inst_HQ_3）的可用性。",
        },
        ApiEndpoint {
            category: CAT_SEPARATION,
            method: "GET",
            path: "/v1/separation/result",
            summary: "分离结果",
            description: "?key= 读取 separation.json，附人声 / 伴奏文件的路径、存在性与大小。",
        },
        ApiEndpoint {
            category: CAT_SEPARATION,
            method: "POST",
            path: "/v1/separation",
            summary: "提交人声分离任务",
            description: "请求体 {\"key\":…,\"model\":…,\"target\":\"vocals\"|\"instrumental\"}；返回 202 与 task。",
        },
        ApiEndpoint {
            category: CAT_MODELS,
            method: "GET",
            path: "/v1/models",
            summary: "模型清单与状态",
            description: "扫描模型目录，返回可加载的 ASR 模型、已加载模型与最近一次加载状态。",
        },
        ApiEndpoint {
            category: CAT_MODELS,
            method: "POST",
            path: "/v1/models/load",
            summary: "提交模型加载任务",
            description: "请求体 {\"id\":\"模型 id（见 /v1/models）\"}；返回 202 与 task。",
        },
        ApiEndpoint {
            category: CAT_MODELS,
            method: "POST",
            path: "/v1/models/unload",
            summary: "释放模型",
            description: "释放已加载的 ASR 模型并复位加载状态。",
        },
        ApiEndpoint {
            category: CAT_TASKS,
            method: "GET",
            path: "/v1/tasks",
            summary: "任务列表",
            description: "?kind=subtitles|separation|asrLoad&status=queued|running|succeeded|failed&limit=50。",
        },
        ApiEndpoint {
            category: CAT_TASKS,
            method: "GET",
            path: "/v1/tasks/{id}",
            summary: "任务详情",
            description: "返回状态、阶段、进度与结果；完成后 result 含完整交付物。",
        },
        ApiEndpoint {
            category: CAT_TASKS,
            method: "GET",
            path: "/v1/tasks/{id}/events",
            summary: "任务进度流（SSE）",
            description: "event 为 snapshot / progress / done / failed，data 为任务快照；收到终态后客户端应关闭连接。",
        },
        ApiEndpoint {
            category: CAT_COMMAND,
            method: "POST",
            path: "/v1/command/{action}",
            summary: "通用命令",
            description: "把 action 直接转给界面执行，参数放在 params 里；未知 action 返回 404。",
        },
    ]
}

/// 分类 + 接口一次返回；「第三方接入」直接按分类分组渲染。
pub fn catalog() -> ApiCatalog {
    ApiCatalog {
        categories: categories(),
        endpoints: endpoints(),
    }
}
