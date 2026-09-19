import { tokenCounterForModel } from './model-token-counter.mjs';
import { TokenRateMeter } from './token-rate-meter.mjs';

const DEFAULT_SAMPLE_INTERVAL_MS = 250;
const DEFAULT_STALE_MS = 1_500;
const DEFAULT_DISPLAY_INTERVAL_MS = 250;
const MIN_OBSERVATION_MS = 500;
const DEFAULT_METADATA_REFRESH_MS = 2_000;
const DEFAULT_METADATA_TTL_MS = 5_000;

export class LiveTpsReporter {
  constructor({
    publisher,
    sampleIntervalMs = DEFAULT_SAMPLE_INTERVAL_MS,
    staleMs = DEFAULT_STALE_MS,
    displayIntervalMs = DEFAULT_DISPLAY_INTERVAL_MS,
    metadataRefreshMs = DEFAULT_METADATA_REFRESH_MS,
    metadataTtlMs = DEFAULT_METADATA_TTL_MS,
    tokenCounterFactory = tokenCounterForModel,
    autoStart = true,
  } = {}) {
    this.publisher = publisher;
    this.sampleIntervalMs = sampleIntervalMs;
    this.staleMs = staleMs;
    this.displayIntervalMs = displayIntervalMs;
    this.resetDisplay();
    this.metadataRefreshMs = metadataRefreshMs;
    this.metadataTtlMs = metadataTtlMs;
    this.sampler = new TokenRateMeter({
      countTokens: tokenCounterFactory(undefined),
    });
    this.tokenCounterFactory = tokenCounterFactory;
    this.currentModel = undefined;
    this.displayAgent = undefined;
    this.lastRate = 0;
    this.timer = undefined;
    this.metadataTimer = undefined;
    this.closePromise = undefined;
    this.closed = false;

    void this.publisher.resetForAgent(this.metadataTtlMs);

    if (autoStart) {
      this.timer = setInterval(() => this.tick(), sampleIntervalMs);
      this.timer.unref?.();
      this.metadataTimer = setInterval(() => this.refreshMetadata(), metadataRefreshMs);
      this.metadataTimer.unref?.();
    }
  }

  setModel(model, now = Date.now()) {
    if (this.closed) return;
    if (!model || model === this.currentModel) {
      return;
    }
    this.sampler.start(this.sampler.generationKey, now);
    this.publishZero();
    this.currentModel = model;
    void this.publisher.publishModel(model, this.metadataTtlMs);
    this.sampler.configure({ countTokens: this.tokenCounterFactory(model) }, now);
  }

  setDisplayAgent(displayAgent) {
    if (this.closed) return;
    if (!displayAgent || displayAgent === this.displayAgent) {
      return;
    }
    this.displayAgent = displayAgent;
    void this.publisher.publishDisplayAgent(displayAgent, this.metadataTtlMs);
  }

  resetSession(now = Date.now()) {
    if (this.closed) return;
    this.currentModel = undefined;
    this.sampler.start(undefined, now);
    this.sampler.configure({ countTokens: this.tokenCounterFactory(undefined) }, now);
    this.publishZero();
    void this.publisher.clearModel();
  }

  refreshDisplayAgent() {
    if (this.closed) return;
    if (!this.displayAgent) {
      return;
    }
    void this.publisher.publishDisplayAgent(this.displayAgent, this.metadataTtlMs);
  }

  refreshMetadata() {
    if (this.closed) return;
    const snapshot = {
      model: this.currentModel,
      displayAgent: this.displayAgent,
      rate: this.lastRate,
    };
    const ttlMs =
      this.lastRate === undefined
        ? this.metadataTtlMs
        : Math.min(this.metadataTtlMs, this.rateTtlMs(this.lastRate));
    void this.publisher.publishSnapshot(snapshot, ttlMs);
  }

  start(generationKey, model, now = Date.now()) {
    if (this.closed) return;
    this.setModel(model, now);
    this.sampler.start(generationKey, now);
    this.publishZero();
  }

  append(generationKey, streamKey, delta, model, now = Date.now()) {
    if (this.closed) return;
    if (typeof delta !== 'string' || delta.length === 0) return;
    this.setModel(model, now);
    if (
      this.sampler.generationKey !== generationKey ||
      this.sampler.lastDeltaAt === undefined ||
      now - this.sampler.lastDeltaAt >= this.staleMs
    ) {
      this.publishZero();
    }
    this.sampler.append(generationKey, streamKey, delta, now);
  }

  pause(now = Date.now(), fallbackDurationMs, outputTokens) {
    if (this.closed) return;
    const observationDuration = this.sampler.observationDuration(now);
    let finalRate = this.sampler.sample(now);
    // One nearly instantaneous chunk has no meaningful streaming-rate denominator.
    if (this.lastRate === 0 && observationDuration < this.sampleIntervalMs) {
      finalRate = undefined;
    }
    const durationMs = Number(fallbackDurationMs);
    if (
      (finalRate === undefined || finalRate === null || finalRate <= 0) &&
      Number.isFinite(durationMs) &&
      durationMs > 0
    ) {
      finalRate = Math.round((this.sampler.totalTokens() / durationMs) * 1_000);
    }
    if (finalRate === undefined || finalRate === null || finalRate <= 0) {
      this.sampler.pause(now, fallbackDurationMs, outputTokens);
      this.publishZero();
      return;
    }

    // Preserve the readable streaming value; completion must not introduce a spike.
    if (this.lastRate === 0) this.publishRate(finalRate);
    this.sampler.pause(now, fallbackDurationMs, outputTokens);
  }

  seed(tokens, durationMs) {
    if (this.closed) return;
    this.sampler.seed(tokens, durationMs);
    const rate = this.sampler.sample();
    if (rate !== undefined && rate > 0) {
      this.publishRate(Math.max(1, rate));
    }
  }


  publishZero() {
    this.resetDisplay();
    if (this.lastRate === 0) {
      return;
    }
    this.lastRate = 0;
    void this.publisher.publishRate(0, this.metadataTtlMs);
  }

  tick(now = Date.now()) {
    if (this.closed) return;
    const rate = this.sampler.sample(now);
    if (rate === undefined || rate <= 0) {
      this.publishZero();
      return;
    }
    if (this.sampler.observationDuration(now) < MIN_OBSERVATION_MS) return;
    this.smoothedRate = Math.round(rate * 10) / 10;
    this.lastSmoothedAt = now;
    if (this.lastDisplayAt !== undefined && now - this.lastDisplayAt < this.displayIntervalMs)
      return;
    const displayRate = Math.max(1, this.smoothedRate);
    if (this.lastRate > 0 && Math.round(this.lastRate * 10) === Math.round(displayRate * 10)) {
      return;
    }
    this.lastDisplayAt = now;
    this.publishRate(displayRate);
  }

  resetDisplay() {
    this.smoothedRate = undefined;
    this.lastSmoothedAt = undefined;
    this.lastDisplayAt = undefined;
  }

  publishRate(displayRate) {
    if (displayRate === this.lastRate) {
      return;
    }
    this.lastRate = displayRate;
    void this.publisher.publishRate(displayRate, this.rateTtlMs(displayRate));
  }

  rateTtlMs(displayRate) {
    return displayRate === 0
      ? this.metadataTtlMs
      : Math.max(this.staleMs, this.metadataRefreshMs) + this.sampleIntervalMs * 2;
  }

  close() {
    if (this.closePromise) {
      return this.closePromise;
    }
    this.closed = true;
    this.sampler.start(undefined);
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = undefined;
    }
    if (this.metadataTimer) {
      clearInterval(this.metadataTimer);
      this.metadataTimer = undefined;
    }
    this.lastRate = undefined;
    this.currentModel = undefined;
    this.displayAgent = undefined;
    this.closePromise = this.publisher.clearAll();
    return this.closePromise;
  }
}
