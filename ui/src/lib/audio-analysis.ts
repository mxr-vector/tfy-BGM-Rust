/**
 * 整曲离线分析：响度、削波、瞬态突变、静音段与立体声相位。
 *
 * 只在用户打开诊断面板时运行一次，直接消费已经解码好的 AudioBuffer，
 * 不重新读文件。K-weighting 使用 ITU-R BS.1770 的参数，在任意采样率下现算系数。
 */

export interface SilenceSegment {
  start: number;
  end: number;
}

export interface AudioAnalysis {
  duration: number;
  sampleRate: number;
  channels: number;
  integratedLufs: number | null;
  rmsDb: number | null;
  peakDb: number | null;
  clippedSamples: number;
  clipRuns: number;
  pops: number;
  maxDelta: number;
  silence: SilenceSegment[];
  minCorrelation: number | null;
  averageCorrelation: number | null;
  outOfPhase: boolean;
  issues: string[];
  verdict: 'good' | 'warning' | 'bad';
}

interface Biquad {
  b0: number;
  b1: number;
  b2: number;
  a1: number;
  a2: number;
}

const CLIP_LEVEL = 0.999;
const CLIP_RUN_LEVEL = 0.9;
const POP_DELTA = 0.65;
/** 相邻采样差的滑动均值衰减率；用于把爆音判定变成局部离群检测。 */
const POP_AVERAGE_RATE = 0.01;
const POP_AVERAGE_RATIO = 6;
const SILENCE_DB = -50;
const SILENCE_MIN_SECONDS = 0.5;

function highShelf(sampleRate: number): Biquad {
  const f0 = 1681.974450955533;
  const gainDb = 3.999843853973347;
  const q = 0.7071752369554196;
  const A = 10 ** (gainDb / 40);
  const w0 = (2 * Math.PI * f0) / sampleRate;
  const cosw = Math.cos(w0);
  const alpha = Math.sin(w0) / (2 * q);
  const sqrtA = Math.sqrt(A);

  const b0 = A * (A + 1 + (A - 1) * cosw + 2 * sqrtA * alpha);
  const b1 = -2 * A * (A - 1 + (A + 1) * cosw);
  const b2 = A * (A + 1 + (A - 1) * cosw - 2 * sqrtA * alpha);
  const a0 = A + 1 - (A - 1) * cosw + 2 * sqrtA * alpha;
  const a1 = 2 * (A - 1 - (A + 1) * cosw);
  const a2 = A + 1 - (A - 1) * cosw - 2 * sqrtA * alpha;
  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
}

function highPass(sampleRate: number): Biquad {
  const f0 = 38.13547087602444;
  const q = 0.5003270373238773;
  const w0 = (2 * Math.PI * f0) / sampleRate;
  const cosw = Math.cos(w0);
  const alpha = Math.sin(w0) / (2 * q);

  const b0 = (1 + cosw) / 2;
  const b1 = -(1 + cosw);
  const b2 = (1 + cosw) / 2;
  const a0 = 1 + alpha;
  const a1 = -2 * cosw;
  const a2 = 1 - alpha;
  return { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 };
}

function db(value: number): number {
  return value > 0 ? 20 * Math.log10(value) : Number.NEGATIVE_INFINITY;
}

export function analyseBuffer(buffer: AudioBuffer): AudioAnalysis {
  const { sampleRate, numberOfChannels: channels, length: totalSamples, duration } = buffer;
  // BS.1770 积分响度：400ms 窗、100ms 步进（75% 重叠）。
  const lufsHop = Math.max(1, Math.round(sampleRate * 0.1));
  const lufsWindow = Math.max(1, Math.round(sampleRate * 0.4));
  const rmsBlock = Math.max(1, Math.round(sampleRate * 0.1));
  const lufsSteps = Math.ceil(totalSamples / lufsHop);
  const rmsBlocks = Math.ceil(totalSamples / rmsBlock);
  const lufsEnergy = new Float64Array(lufsSteps);
  const rmsEnergy = new Float64Array(rmsBlocks);

  let peak = 0;
  let clippedSamples = 0;
  let clipRuns = 0;
  let pops = 0;
  let maxDelta = 0;

  const shelf = highShelf(sampleRate);
  const hp = highPass(sampleRate);

  for (let channel = 0; channel < channels; channel += 1) {
    const data = buffer.getChannelData(channel);
    let sx1 = 0;
    let sx2 = 0;
    let sy1 = 0;
    let sy2 = 0;
    let hx1 = 0;
    let hx2 = 0;
    let hy1 = 0;
    let hy2 = 0;
    let previous = 0;
    let previousDelta = 0;
    let averageDelta = 0;
    let clipping = false;
    let runCounted = false;

    for (let index = 0; index < data.length; index += 1) {
      const sample = data[index]!;
      const magnitude = Math.abs(sample);
      if (magnitude > peak) peak = magnitude;

      if (magnitude >= CLIP_LEVEL) {
        clippedSamples += 1;
        if (clipping && !runCounted) {
          clipRuns += 1;
          runCounted = true;
        }
        clipping = true;
      } else {
        clipping = false;
        runCounted = false;
      }

      const delta = Math.abs(sample - previous);
      if (delta > maxDelta) maxDelta = delta;
      // 只有明显高于局部平均跳变的孤立跳变才算爆音，避免高频/噪声素材误报。
      const popThreshold = Math.max(POP_DELTA, averageDelta * POP_AVERAGE_RATIO);
      if (delta > popThreshold && previousDelta <= popThreshold && magnitude < CLIP_RUN_LEVEL) {
        pops += 1;
      }
      averageDelta += (delta - averageDelta) * POP_AVERAGE_RATE;
      previousDelta = delta;
      previous = sample;

      rmsEnergy[Math.floor(index / rmsBlock)]! += sample * sample;

      // K-weighting：先高架再高通，按 100ms 步进累计能量。
      const shaped =
        shelf.b0 * sample +
        shelf.b1 * sx1 +
        shelf.b2 * sx2 -
        shelf.a1 * sy1 -
        shelf.a2 * sy2;
      sx2 = sx1;
      sx1 = sample;
      sy2 = sy1;
      sy1 = shaped;
      const weighted =
        hp.b0 * shaped + hp.b1 * hx1 + hp.b2 * hx2 - hp.a1 * hy1 - hp.a2 * hy2;
      hx2 = hx1;
      hx1 = shaped;
      hy2 = hy1;
      hy1 = weighted;
      lufsEnergy[Math.floor(index / lufsHop)]! += weighted * weighted;
    }
  }

  // 每个 400ms 窗的能量按声道求和（BS.1770 双声道权重均为 1）；不足一窗的尾部忽略。
  const stepsPerWindow = Math.max(1, Math.round(lufsWindow / lufsHop));
  const lufsBlocks: number[] = [];
  for (let start = 0; start + stepsPerWindow <= lufsSteps; start += 1) {
    if ((start + stepsPerWindow) * lufsHop > totalSamples) break;
    let sum = 0;
    for (let step = 0; step < stepsPerWindow; step += 1) {
      sum += lufsEnergy[start + step]!;
    }
    lufsBlocks.push(sum / (stepsPerWindow * lufsHop));
  }
  let rawTotal = 0;
  for (const energy of rmsEnergy) rawTotal += energy;
  const rms = totalSamples > 0 ? rawTotal / (totalSamples * channels) : 0;
  const integratedLufs = integratedLoudness(lufsBlocks);

  const silence = quietSegments(rmsEnergy, rmsBlock, sampleRate, channels);
  const phase = stereoPhase(buffer);

  const peakDb = peak > 0 ? db(peak) : null;
  const issues: string[] = [];
  let severity = 0;

  if (clippedSamples > 0) {
    issues.push(
      `存在削波：${clippedSamples} 个采样达到满幅（峰值 ${peakDb?.toFixed(1) ?? '—'} dBFS）`,
    );
    severity = Math.max(severity, 2);
  }
  if (phase.outOfPhase) {
    issues.push(
      `左右声道相位异常（最低相关 ${phase.min?.toFixed(2) ?? '—'}），单声道播放可能抵消`,
    );
    severity = Math.max(severity, 2);
  }
  if (silence.length > 0) {
    const totalSilence = silence.reduce((sum, segment) => sum + (segment.end - segment.start), 0);
    issues.push(`含 ${silence.length} 段静音，共 ${totalSilence.toFixed(1)} 秒`);
    severity = Math.max(severity, totalSilence > 5 ? 2 : 1);
  }
  if (integratedLufs !== null && (integratedLufs > -9 || integratedLufs < -24)) {
    issues.push(`整体响度 ${integratedLufs.toFixed(1)} LUFS 偏离常见 BGM 区间（-24 ~ -9）`);
    severity = Math.max(severity, 1);
  }
  if (pops > 40) {
    issues.push(`检测到 ${pops} 次疑似瞬态爆音/跳变`);
    severity = Math.max(severity, 1);
  }

  return {
    duration,
    sampleRate,
    channels,
    integratedLufs,
    rmsDb: Number.isFinite(rms) && rms > 0 ? 10 * Math.log10(rms) : null,
    peakDb,
    clippedSamples,
    clipRuns,
    pops,
    maxDelta,
    silence,
    minCorrelation: phase.min,
    averageCorrelation: phase.average,
    outOfPhase: phase.outOfPhase,
    issues,
    verdict: severity >= 2 ? 'bad' : severity === 1 ? 'warning' : 'good',
  };
}

function integratedLoudness(blocks: readonly number[]): number | null {
  const loudness: number[] = [];
  const energies: number[] = [];
  for (const energy of blocks) {
    if (energy <= 0) continue;
    const value = -0.691 + 10 * Math.log10(energy);
    if (value > -70) {
      loudness.push(value);
      energies.push(energy);
    }
  }
  if (energies.length === 0) return null;

  const mean = energies.reduce((sum, value) => sum + value, 0) / energies.length;
  const relativeGate = -0.691 + 10 * Math.log10(mean) - 10;
  let sum = 0;
  let count = 0;
  for (let index = 0; index < energies.length; index += 1) {
    if (loudness[index]! > relativeGate) {
      sum += energies[index]!;
      count += 1;
    }
  }
  if (count === 0) return null;
  return -0.691 + 10 * Math.log10(sum / count);
}

function quietSegments(
  energy: Float64Array,
  blockSamples: number,
  sampleRate: number,
  channels: number,
): SilenceSegment[] {
  const segments: SilenceSegment[] = [];
  const threshold = 10 ** (SILENCE_DB / 10);
  const blockSeconds = blockSamples / sampleRate;
  const length = energy.length * blockSamples;
  let start: number | null = null;

  energy.forEach((sum, index) => {
    const samples = Math.min(blockSamples, length - index * blockSamples);
    const mean = samples > 0 ? sum / (samples * channels) : 0;
    if (mean < threshold) {
      if (start === null) start = index * blockSeconds;
    } else if (start !== null) {
      const end = index * blockSeconds;
      if (end - start >= SILENCE_MIN_SECONDS) segments.push({ start, end });
      start = null;
    }
  });
  if (start !== null) {
    const end = (energy.length * blockSamples) / sampleRate;
    if (end - start >= SILENCE_MIN_SECONDS) segments.push({ start, end });
  }
  return segments;
}

function stereoPhase(buffer: AudioBuffer): {
  min: number | null;
  average: number | null;
  outOfPhase: boolean;
} {
  if (buffer.numberOfChannels < 2) return { min: null, average: null, outOfPhase: false };
  const left = buffer.getChannelData(0);
  const right = buffer.getChannelData(1);
  const block = Math.max(1, Math.round(buffer.sampleRate));

  let minimum = Number.POSITIVE_INFINITY;
  let sum = 0;
  let count = 0;
  let outOfPhase = false;

  for (let start = 0; start < left.length; start += block) {
    const end = Math.min(start + block, left.length);
    const samples = end - start;
    if (samples < block / 2) continue;

    let sumX = 0;
    let sumY = 0;
    let sumXX = 0;
    let sumYY = 0;
    let sumXY = 0;
    for (let index = start; index < end; index += 1) {
      const x = left[index]!;
      const y = right[index]!;
      sumX += x;
      sumY += y;
      sumXX += x * x;
      sumYY += y * y;
      sumXY += x * y;
    }
    const meanX = sumX / samples;
    const meanY = sumY / samples;
    const varX = sumXX / samples - meanX * meanX;
    const varY = sumYY / samples - meanY * meanY;
    if (varX < 1e-8 || varY < 1e-8) continue;
    const covariance = sumXY / samples - meanX * meanY;
    const correlation = covariance / Math.sqrt(varX * varY);
    if (!Number.isFinite(correlation)) continue;
    minimum = Math.min(minimum, correlation);
    sum += correlation;
    count += 1;
    if (correlation < -0.25) outOfPhase = true;
  }

  return {
    min: count > 0 ? minimum : null,
    average: count > 0 ? sum / count : null,
    outOfPhase,
  };
}

/** 整曲波形峰值（对称包络），用于诊断面板的静态波形。 */
export function computePeaks(buffer: AudioBuffer, buckets: number): Float32Array {
  const count = Math.max(1, Math.floor(buckets));
  const peaks = new Float32Array(count);
  const length = buffer.length;
  for (let channel = 0; channel < buffer.numberOfChannels; channel += 1) {
    const data = buffer.getChannelData(channel);
    for (let bucket = 0; bucket < count; bucket += 1) {
      const start = Math.floor((bucket * length) / count);
      const end = Math.floor(((bucket + 1) * length) / count);
      let maximum = 0;
      for (let index = start; index < end; index += 1) {
        const magnitude = Math.abs(data[index]!);
        if (magnitude > maximum) maximum = magnitude;
      }
      if (maximum > peaks[bucket]!) peaks[bucket] = maximum;
    }
  }
  return peaks;
}