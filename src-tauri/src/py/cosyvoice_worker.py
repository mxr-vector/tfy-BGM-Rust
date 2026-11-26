"""CosyVoice3 全 ONNX 推理 worker（非流式）。

协议：stdin 每行一个 JSON 请求；stdout 输出 TFY_COSYVOICE 前缀的 JSON 回复。
参考语音特征沿用 CosyVoice 前端（Tokenizer/campplus/speech_tokenizer），
LLM、Flow、HiFT 全部走 ONNX Runtime，优先使用 DirectML/CUDA 显卡。
"""
import os
import sys

# 为协议保留原始 stdout；在导入运行库前把 Python 与原生库的 stdout 重定向到 stderr。
protocol = os.fdopen(os.dup(sys.stdout.fileno()), 'w', encoding='ascii', newline='\n')
os.dup2(sys.stderr.fileno(), sys.stdout.fileno())
sys.stdout = sys.stderr

import hashlib
import json
import math
import subprocess
import traceback

# 禁用 wetext：它初始化时要联网下载模型，而本 worker 强制离线运行；
# CosyVoice 前端的 text_frontend 会回退为空，改用内置的标点/数字处理。
sys.modules['wetext'] = None

MEL_CHANNELS = 80
FLOW_HEAD_DIM = 64
FLOW_STEPS = 10
FLOW_CFG_RATE = 0.7
FLOW_LOOKAHEAD = 3
TOKEN_MEL_RATIO = 2
SAMPLES_PER_MEL = 480
HARMONIC_CHANNELS = 9
NOISE_SEED = 1986
MAX_NOISE_FRAMES = 50 * 300
SPEECH_TOKEN_SIZE = 6561
SOS_TOKEN = SPEECH_TOKEN_SIZE
EOS_TOKEN = SPEECH_TOKEN_SIZE + 1
TASK_ID_TOKEN = SPEECH_TOKEN_SIZE + 2
STOP_TOKEN_IDS = {SPEECH_TOKEN_SIZE + index for index in range(200)}
TEXT_TOKEN_TYPE = 0
SPEECH_TOKEN_TYPE = 1
IGNORE_EOS = EOS_TOKEN


def reply(kind, **fields):
    protocol.write('TFY_COSYVOICE ' + json.dumps(dict(kind=kind, **fields)) + '\n')
    protocol.flush()


def log(message):
    print('[cosyvoice] ' + str(message), file=sys.stderr, flush=True)


def pin_high_performance():
    """把进程绑定到大小核里的 P 核。

    DirectML 每个算子的调度都在主线程上同步提交；线程一旦被调度到 E 核，
    逐 token 解码会慢 2-3 倍。这里读取 Windows 的 CPU Set EfficiencyClass，
    只在大核存在时收紧亲和性，避免影响普通机器。
    """
    if os.name != 'nt':
        return
    try:
        import ctypes
        from ctypes import wintypes
        kernel32 = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel32.GetCurrentProcess.restype = wintypes.HANDLE
        get_info = kernel32.GetSystemCpuSetInformation
        get_info.restype = wintypes.BOOL
        length = wintypes.ULONG(0)
        get_info(None, 0, ctypes.byref(length), None, 0)
        if not length.value:
            return
        buffer = ctypes.create_string_buffer(length.value)
        if not get_info(buffer, length.value, ctypes.byref(length), None, 0):
            return
        entries = []
        offset = 0
        while offset + 20 <= length.value:
            size = ctypes.c_ulong.from_buffer(buffer, offset).value
            if size < 20:
                break
            if ctypes.c_ulong.from_buffer(buffer, offset + 4).value == 0:
                group = ctypes.c_ushort.from_buffer(buffer, offset + 12).value
                logical = ctypes.c_ubyte.from_buffer(buffer, offset + 14).value
                efficiency = ctypes.c_ubyte.from_buffer(buffer, offset + 18).value
                entries.append((efficiency, group, logical))
            offset += size
        if not entries:
            return
        best = max(entry[0] for entry in entries)
        if all(entry[0] == best for entry in entries):
            return
        mask = 0
        count = 0
        for efficiency, group, logical in entries:
            if efficiency == best and group == 0:
                mask |= 1 << logical
                count += 1
        if not mask:
            return
        # SetProcessAffinityMask 只覆盖单一处理器组（<=64 逻辑核心），已覆盖绝大多数机器。
        kernel32.SetProcessAffinityMask.argtypes = [wintypes.HANDLE, ctypes.c_size_t]
        kernel32.SetProcessAffinityMask.restype = wintypes.BOOL
        if kernel32.SetProcessAffinityMask(kernel32.GetCurrentProcess(), mask):
            log(f'已绑定 {count} 个性能核')
    except Exception as exc:
        log(f'性能核绑定失败：{exc}')


def thread_count():
    return max(1, min(os.cpu_count() or 4, 8))


def gpu_free_mib():
    try:
        completed = subprocess.run(
            ['nvidia-smi', '--query-gpu=memory.free', '--format=csv,noheader,nounits'],
            capture_output=True, text=True, timeout=5, check=False)
    except Exception:
        return 0
    if completed.returncode != 0:
        return 0
    values = [int(line) for line in completed.stdout.split() if line.isdigit()]
    return max(values, default=0)


def gpu_providers(ort):
    available = ort.get_available_providers()
    providers = []
    if 'CUDAExecutionProvider' in available:
        providers.append(('CUDAExecutionProvider', {'device_id': 0}))
    if 'DmlExecutionProvider' in available:
        providers.append('DmlExecutionProvider')
    return providers


def make_session(path, use_gpu, intra=1):
    import onnxruntime as ort
    options = ort.SessionOptions()
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    options.intra_op_num_threads = intra
    options.inter_op_num_threads = 1
    providers = gpu_providers(ort) if use_gpu else []
    providers.append('CPUExecutionProvider')
    try:
        session = ort.InferenceSession(path, sess_options=options, providers=providers)
    except Exception as exc:
        if providers[0] == 'CPUExecutionProvider':
            raise
        log(f'{os.path.basename(path)} GPU 初始化失败，改用 CPU：{exc}')
        session = ort.InferenceSession(path, sess_options=options,
                                       providers=['CPUExecutionProvider'])
    log(f'{os.path.basename(path)} providers={session.get_providers()}')
    return session


def enable_cuda_dlls(torch):
    lib = os.path.join(os.path.dirname(torch.__file__), 'lib')
    if not os.path.isdir(lib):
        return
    os.environ['PATH'] = lib + os.pathsep + os.environ.get('PATH', '')
    add_dll_directory = getattr(os, 'add_dll_directory', None)
    if add_dll_directory is not None:
        try:
            add_dll_directory(lib)
        except OSError:
            pass


def load_config(model_dir):
    from hyperpyyaml import load_hyperpyyaml
    with open(os.path.join(model_dir, 'cosyvoice3.yaml'), encoding='utf-8') as config_file:
        return load_hyperpyyaml(config_file, overrides={
            'qwen_pretrain_path': os.path.join(model_dir, 'CosyVoice-BlankEN'),
            'llm': None, 'flow': None, 'hift': None, 'hifigan': None,
        })


def causal_mask(new_length, past_length):
    import numpy as np
    total = past_length + new_length
    rows = np.arange(new_length, dtype=np.int64).reshape(new_length, 1)
    columns = np.arange(total, dtype=np.int64).reshape(1, total)
    blocked = columns > past_length + rows
    mask = np.zeros((new_length, total), dtype=np.float16)
    mask[blocked] = np.finfo(np.float16).min
    return mask.reshape(1, 1, new_length, total)


def ras_sampling(scores, decoded, rng, top_p=0.8, top_k=25, win_size=10, tau_r=0.1):
    """与上游一致的重复感知采样（NumPy 实现）。"""
    import numpy as np
    adjusted = scores.astype(np.float32).copy()
    order = np.argsort(-adjusted)
    candidates = order[:top_k]
    probs = np.exp(adjusted[candidates] - adjusted[candidates[0]])
    probs /= probs.sum()
    cumulative = np.cumsum(probs)
    cutoff = int(np.searchsorted(cumulative, top_p) + 1)
    candidates = candidates[:max(1, cutoff)]
    probs = probs[:len(candidates)]
    probs /= probs.sum()
    chosen = int(rng.choice(candidates, p=probs))
    recent = np.asarray(decoded[-win_size:], dtype=np.int64)
    repeats = int(np.count_nonzero(recent == chosen))
    if repeats >= win_size * tau_r:
        adjusted[chosen] = -np.inf
        shifted = adjusted - adjusted.max()
        weights = np.exp(shifted)
        weights /= weights.sum()
        chosen = int(rng.choice(len(weights), p=weights))
    return chosen


class LlmOnnx:
    def __init__(self, path, use_gpu, intra):
        self.session = make_session(path, use_gpu, intra)
        names = [item.name for item in self.session.get_inputs()]
        self.past_names = sorted(
            (name for name in names if name.startswith('past_')),
            key=lambda name: (int(name.rsplit('_', 1)[-1]), 0 if 'key' in name else 1))
        self.present_names = sorted(
            (item.name for item in self.session.get_outputs() if item.name.startswith('present_')),
            key=lambda name: (int(name.rsplit('_', 1)[-1]), 0 if 'key' in name else 1))
        if not self.past_names or len(self.past_names) != len(self.present_names):
            raise RuntimeError('LLM ONNX 的 KV Cache 输入输出不匹配')
        logits = next(item for item in self.session.get_outputs() if item.name == 'logits')
        if int(logits.shape[-1]) != SPEECH_TOKEN_SIZE + 200:
            raise RuntimeError(f'LLM 词表大小异常：{logits.shape[-1]}')
        # DirectML 在一次多 token 前向之后，同一 session 的逐 token 解码会明显变慢
        # （约 2-3 倍）。前向填充与自回归解码分开用两个 session，可让解码始终
        # 走快路径；CPU 没有这个现象，保持单 session 以节省内存。
        if not use_gpu:
            self.decode_session = self.session
        else:
            decode = make_session(path, use_gpu, intra)
            if {'CUDAExecutionProvider', 'DmlExecutionProvider'} & set(decode.get_providers()):
                self.decode_session = decode
            else:
                # 显存不足时第二个会话会回退 CPU，这比共享前一个会话还慢。
                log('第二个 LLM 会话未获得显卡，回退为单会话解码')
                self.decode_session = self.session

    def step(self, session, token_ids, token_types, past):
        import numpy as np
        new_length = token_ids.shape[1]
        past_length = 0 if past is None else past[0].shape[2]
        position_ids = np.arange(past_length, past_length + new_length,
                                 dtype=np.int64).reshape(1, new_length)
        feeds = {
            'token_ids': token_ids,
            'token_types': token_types,
            'attention_mask': causal_mask(new_length, past_length),
            'position_ids': position_ids,
        }
        if past is None:
            empty = np.zeros((1, 2, 0, FLOW_HEAD_DIM), dtype=np.float16)
            feeds.update({name: empty for name in self.past_names})
        else:
            feeds.update(dict(zip(self.past_names, past)))
        outputs = session.run(['logits', *self.present_names], feeds)
        return outputs[0], outputs[1:]

    def infer(self, text, prompt_text, prompt_speech, seed=NOISE_SEED, min_ratio=2, max_ratio=20):
        import numpy as np
        rng = np.random.default_rng(seed)
        token_ids = np.concatenate([
            np.array([[SOS_TOKEN]], dtype=np.int64),
            prompt_text,
            text,
            np.array([[TASK_ID_TOKEN]], dtype=np.int64),
            prompt_speech,
        ], axis=1)
        token_types = np.concatenate([
            np.array([[SPEECH_TOKEN_TYPE]], dtype=np.int64),
            np.full(prompt_text.shape, TEXT_TOKEN_TYPE, dtype=np.int64),
            np.full(text.shape, TEXT_TOKEN_TYPE, dtype=np.int64),
            np.array([[SPEECH_TOKEN_TYPE]], dtype=np.int64),
            np.full(prompt_speech.shape, SPEECH_TOKEN_TYPE, dtype=np.int64),
        ], axis=1)
        minimum = int(text.shape[1] * min_ratio)
        maximum = int(text.shape[1] * max_ratio)
        decoded = []
        logits, past = self.step(self.session, token_ids, token_types, None)
        for index in range(maximum):
            scores = logits[0, -1].astype(np.float32)
            scores = scores - scores.max()
            scores -= np.log(np.exp(scores).sum())
            if index < minimum:
                scores[IGNORE_EOS] = -np.inf
            token = ras_sampling(scores, decoded, rng)
            if token in STOP_TOKEN_IDS:
                break
            decoded.append(token)
            token_ids = np.array([[token]], dtype=np.int64)
            token_types = np.array([[SPEECH_TOKEN_TYPE]], dtype=np.int64)
            logits, past = self.step(self.decode_session, token_ids, token_types, past)
        if not decoded:
            raise RuntimeError('LLM 没有生成任何语音 Token')
        return decoded


class FlowOnnx:
    def __init__(self, conditioner_path, dit_path, use_gpu, intra):
        self.conditioner = make_session(conditioner_path, use_gpu, intra)
        self.dit = make_session(dit_path, use_gpu, intra)
        self._noise = None

    def _noise_prefix(self, frames):
        import numpy as np
        if self._noise is None:
            self._noise = np.random.default_rng(0).standard_normal(
                (1, MEL_CHANNELS, MAX_NOISE_FRAMES)).astype(np.float32)
        return np.ascontiguousarray(self._noise[:, :, :frames])

    def _rope(self, frames):
        import numpy as np
        positions = np.arange(frames, dtype=np.float32)
        dimensions = np.arange(0, FLOW_HEAD_DIM, 2, dtype=np.float32)
        inverse = 1.0 / (10000.0 ** (dimensions / FLOW_HEAD_DIM))
        frequencies = positions[:, None] * inverse[None, :]
        rope = np.repeat(frequencies, 2, axis=-1)
        return rope.reshape(1, frames, FLOW_HEAD_DIM).astype(np.float32)

    def infer(self, tokens, prompt_tokens, prompt_feat, embedding):
        import numpy as np
        tokens = np.ascontiguousarray(tokens, dtype=np.int64)
        prompt_tokens = np.ascontiguousarray(prompt_tokens, dtype=np.int64)
        prompt_feat = np.ascontiguousarray(prompt_feat, dtype=np.float32)
        embedding = np.ascontiguousarray(embedding, dtype=np.float32)
        mu, speaker = self.conditioner.run(['mu', 'speaker'], {
            'token': tokens,
            'lookahead_token': np.zeros((1, FLOW_LOOKAHEAD), dtype=np.int64),
            'lookahead_enabled': np.zeros((1,), dtype=np.float32),
            'prompt_token': prompt_tokens,
            'embedding': embedding,
        })
        frames = mu.shape[2]
        prompt_frames = prompt_feat.shape[1]
        if prompt_frames > frames:
            raise RuntimeError('参考 Mel 长于 Flow 条件长度')
        condition = np.concatenate([
            np.ascontiguousarray(prompt_feat.transpose(0, 2, 1)),
            np.zeros((1, MEL_CHANNELS, frames - prompt_frames), dtype=np.float32),
        ], axis=2)
        mu_cfg = np.concatenate([mu, np.zeros_like(mu)], axis=0)
        speaker_cfg = np.concatenate([speaker, np.zeros_like(speaker)], axis=0)
        condition_cfg = np.concatenate([condition, np.zeros_like(condition)], axis=0)
        x = self._noise_prefix(frames)
        rope = self._rope(frames)
        bias = np.zeros((1, 1, frames, frames), dtype=np.float32)
        schedule = 1.0 - np.cos(np.linspace(0.0, 1.0, FLOW_STEPS + 1, dtype=np.float32)
                                * (0.5 * math.pi))
        for index in range(FLOW_STEPS):
            velocity = self.dit.run(['velocity'], {
                'x': np.repeat(x, 2, axis=0),
                'mu': mu_cfg,
                'timestep': np.repeat(schedule[index], 2).astype(np.float32),
                'speaker': speaker_cfg,
                'condition': condition_cfg,
                'attention_bias': bias,
                'rope_freqs': rope,
            })[0]
            conditional, unconditional = velocity[:1], velocity[1:]
            guided = (1.0 + FLOW_CFG_RATE) * conditional - FLOW_CFG_RATE * unconditional
            x = x + (schedule[index + 1] - schedule[index]) * guided
        return np.ascontiguousarray(x[:, :, prompt_frames:])


class HiftOnnx:
    def __init__(self, path, use_gpu, intra, seed=NOISE_SEED):
        self.session = make_session(path, use_gpu, intra)
        self.seed = seed

    def infer(self, mel):
        import numpy as np
        mel = np.ascontiguousarray(mel, dtype=np.float32)
        mel_frames = mel.shape[2]
        noise = np.random.default_rng(self.seed).random(
            (1, mel_frames * SAMPLES_PER_MEL, HARMONIC_CHANNELS), dtype=np.float32)
        return self.session.run(['waveform'], {
            'speech_feat': mel,
            'source_noise': noise,
        })[0]


def speaker_key(prompt_path, prompt_text):
    # 参考音频每次合成都会写到新的临时路径，路径与 mtime 不适合做缓存键；
    # 用内容哈希保证同一音色跨请求复用已提取的 prompt 特征。
    digest = hashlib.sha1()
    with open(prompt_path, 'rb') as prompt_file:
        for block in iter(lambda: prompt_file.read(1 << 20), b''):
            digest.update(block)
    digest.update(b'\x00')
    digest.update(prompt_text.encode('utf-8'))
    return 'tfy:' + digest.hexdigest()


def build_runtime(model_dir):
    import torch
    enable_cuda_dlls(torch)
    import onnxruntime as ort
    free_mib = gpu_free_mib()
    minimum = int(os.environ.get('TFY_COSYVOICE_MIN_VRAM_MIB', '6144'))
    use_gpu = free_mib >= minimum and bool(gpu_providers(ort))
    log(f'推理设备：{"GPU" if use_gpu else "CPU"}（可用显存 {free_mib} MiB，阈值 {minimum} MiB）')
    torch.set_num_threads(thread_count())
    from cosyvoice.cli.frontend import CosyVoiceFrontEnd
    config = load_config(model_dir)
    rate = int(config['sample_rate'])
    if rate != 24000:
        raise ValueError(f'expected a 24 kHz CosyVoice3 model, got {rate} Hz')
    frontend = CosyVoiceFrontEnd(
        config['get_tokenizer'], config['feat_extractor'],
        os.path.join(model_dir, 'frontend', 'campplus.onnx'),
        os.path.join(model_dir, 'frontend', 'speech_tokenizer_v3.onnx'),
        allowed_special=config['allowed_special'],
    )
    intra = thread_count()
    # LLM/Flow/HiFT 共用同一套设备决策；单个会话建不起来时 make_session 回退 CPU。
    llm = LlmOnnx(os.path.join(model_dir, 'llm', 'model.onnx'), use_gpu, intra)
    flow = FlowOnnx(os.path.join(model_dir, 'flow', 'flow_conditioner.onnx'),
                    os.path.join(model_dir, 'flow', 'flow_dit_step.onnx'), use_gpu, intra)
    hift = HiftOnnx(os.path.join(model_dir, 'hift', 'model.onnx'), use_gpu, intra)
    log('LLM/Flow/HiFT ONNX 就绪')
    return rate, frontend, llm, flow, hift


def synthesize(frontend, llm, flow, hift, rate, request):
    import numpy as np
    prompt_text = frontend.text_normalize(
        'You are a helpful assistant.<|endofprompt|>' + request['prompt_text'], split=False)
    key = speaker_key(request['prompt_path'], prompt_text)
    if key not in frontend.spk2info:
        entry = frontend.frontend_zero_shot('', prompt_text, request['prompt_path'], rate, '')
        entry.pop('text', None)
        entry.pop('text_len', None)
        frontend.spk2info[key] = entry
    chunks = []
    for text in frontend.text_normalize(request['text'], split=True):
        inputs = frontend.frontend_zero_shot(text, prompt_text, request['prompt_path'], rate, key)
        text_tokens = inputs['text'].detach().cpu().numpy().astype(np.int64, copy=False)
        prompt_text_tokens = inputs['prompt_text'].detach().cpu().numpy().astype(np.int64, copy=False)
        prompt_speech = inputs['llm_prompt_speech_token'].detach().cpu().numpy().astype(np.int64, copy=False)
        prompt_feat = inputs['prompt_speech_feat'].detach().cpu().numpy().astype(np.float32, copy=False)
        embedding = inputs['flow_embedding'].detach().cpu().numpy().astype(np.float32, copy=False)
        tokens = llm.infer(text_tokens, prompt_text_tokens, prompt_speech)
        speech = np.array([tokens], dtype=np.int64)
        mel = flow.infer(speech, prompt_speech, prompt_feat, embedding)
        audio = hift.infer(mel)
        if audio.ndim == 2 and audio.shape[0] == 1:
            audio = audio[0]
        if audio.ndim != 1 or not np.isfinite(audio).all():
            raise ValueError('expected finite mono speech samples')
        chunks.append(audio)
    return np.concatenate(chunks)


def main():
    home, model_dir = sys.argv[1:3]
    sys.path[:0] = [home, os.path.join(home, 'third_party', 'Matcha-TTS')]
    pin_high_performance()
    import soundfile as sf
    rate, frontend, llm, flow, hift = build_runtime(model_dir)
    reply('ready', sample_rate=rate)
    for line in sys.stdin:
        request = json.loads(line)
        audio = synthesize(frontend, llm, flow, hift, rate, request)
        with sf.SoundFile(request['output_path'], mode='w', samplerate=rate,
                          channels=1, format='WAV', subtype='PCM_16') as output:
            output.write(audio)
        info = sf.info(request['output_path'])
        if (info.channels, info.samplerate, info.subtype, info.frames) != (1, rate, 'PCM_16', len(audio)):
            raise ValueError('expected a complete 24 kHz mono PCM16 WAV')
        reply('generated')


def self_test():
    home, model_dir = sys.argv[1:3]
    sys.path[:0] = [home, os.path.join(home, 'third_party', 'Matcha-TTS')]
    pin_high_performance()
    import numpy as np
    rate, frontend, llm, flow, hift = build_runtime(model_dir)
    tokens = np.arange(20, dtype=np.int64)[None, :]
    prompt_tokens = np.arange(4, dtype=np.int64)[None, :]
    mel = flow.infer(tokens, prompt_tokens,
                     np.zeros((1, 8, MEL_CHANNELS), dtype=np.float32),
                     np.ones((1, 192), dtype=np.float32))
    audio = hift.infer(mel)
    if audio.ndim != 2 or audio.shape[0] != 1 or audio.shape[1] <= 0:
        raise ValueError('ONNX 自检输出的波形形状异常')
    log(f'自检通过：Flow/HiFT 输出 {audio.shape}')
    reply('ready', sample_rate=rate)


try:
    if len(sys.argv) > 3 and sys.argv[3] == '--self-test':
        self_test()
    else:
        main()
except Exception as exc:
    traceback.print_exc()
    reply('error', message=f'{type(exc).__name__}: {exc}'[:1024])
    sys.exit(1)
