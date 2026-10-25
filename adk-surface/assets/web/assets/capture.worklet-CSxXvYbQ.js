// The microphone as PCM16 mono at the model's input rate (spec 3.3 §5.6), in 100 ms chunks.
// An AudioWorklet module: it runs on the audio thread and is served from the page's own origin.
class PcmCapture extends AudioWorkletProcessor {
  constructor(options) {
    super();
    this.rate = options.processorOptions.rate;
    this.step = sampleRate / this.rate;
    this.position = 0;
    this.chunk = new Int16Array(Math.round(this.rate / 10));
    this.filled = 0;
  }

  process(inputs) {
    const input = inputs[0] && inputs[0][0];
    if (!input) return true;
    // Linear resampling from the context's rate to the model's.
    for (; this.position < input.length; this.position += this.step) {
      const index = Math.floor(this.position);
      const next = index + 1 < input.length ? input[index + 1] : input[index];
      const sample = Math.max(-1, Math.min(1, input[index] + (next - input[index]) * (this.position - index)));
      this.chunk[this.filled++] = sample < 0 ? sample * 0x8000 : sample * 0x7fff;
      if (this.filled === this.chunk.length) {
        this.port.postMessage(this.chunk.buffer.slice(0));
        this.filled = 0;
      }
    }
    this.position -= input.length;
    return true;
  }
}

registerProcessor("pcm-capture", PcmCapture);
