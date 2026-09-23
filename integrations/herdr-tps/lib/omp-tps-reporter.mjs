import { LiveTpsReporter } from './live-tps-reporter.mjs';
import { TokenRateMeter } from './token-rate-meter.mjs';

// Match OMP's working-row meter, including billed-output reconciliation and
// decimal precision. Codex's burst sampling must not change this policy.
export class OmpTpsReporter extends LiveTpsReporter {
  constructor({ Tokenizer, ...options }) {
    let tokenizer = new Tokenizer();
    const countTokens = (text) => tokenizer.countTokens(text);
    super({ ...options, tokenCounterFactory: () => countTokens });
    this.sampler = new TokenRateMeter({ countTokens, scaleShortResponses: false });
    this.usesOmpNativeSemantics = true;
    let encoding;
    this.configureTokenModel = (model) => {
      if (model?.tokenizer === encoding) return;
      encoding = model?.tokenizer;
      tokenizer = new Tokenizer(model);
    };
  }

  start(key, model, now = Date.now()) {
    if (this.closed) return;
    this.setModel(model, now);
    this.sampler.start(key, now);
    this.tick(now);
  }

  append(key, streamKey, delta, model, now = Date.now()) {
    if (this.closed) return;
    this.setModel(model, now);
    this.sampler.append(key, streamKey, delta, now);
  }

  pause(now = Date.now(), _duration, outputTokens) {
    if (this.closed) return;
    this.sampler.pause(now, undefined, outputTokens);
    // Read AFTER usage reconciliation; retaining the last streaming reading
    // would disagree with OMP throughout the subsequent tool execution.
    this.tick(now);
  }

  tick(now = Date.now()) {
    if (this.closed) return;
    const rate = this.sampler.rate(now);
    this.publishRate(rate === null ? 0 : Number(rate.toFixed(1)));
  }

  seed(tokens, durationMs) {
    if (this.closed) return;
    this.sampler.seed(tokens, durationMs);
    this.tick();
  }

  resetSession(now = Date.now()) {
    this.sampler.reset();
    super.resetSession(now);
  }
}
