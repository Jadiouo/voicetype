// Frozen sherpa 1.13.8 C ABI; no Python, dynamic language setter, or GPU provider.
#include "nano_shim.h"
#include "sherpa-onnx/c-api/c-api.h"
#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstring>
#include <memory>
#include <stdexcept>
#include <string>

struct VtNano {
    const SherpaOnnxOfflineRecognizer *recognizer = nullptr;
    const SherpaOnnxVoiceActivityDetector *vad = nullptr;
    ~VtNano() {
        if (vad) SherpaOnnxDestroyVoiceActivityDetector(vad);
        if (recognizer) SherpaOnnxDestroyOfflineRecognizer(recognizer);
    }
};

namespace {
void error_text(char *out, size_t cap, const char *message) {
    if (out && cap) std::snprintf(out, cap, "%s", message);
}
void validate_audio(const float *samples, size_t count) {
    if ((!samples && count) || count > 16000 * 60)
        throw std::runtime_error("Nano expects at most 60 seconds of 16kHz audio");
    for (size_t i = 0; i < count; ++i)
        if (!std::isfinite(samples[i])) throw std::runtime_error("non-finite audio sample");
}
using Stream = std::unique_ptr<const SherpaOnnxOfflineStream,
                              decltype(&SherpaOnnxDestroyOfflineStream)>;
using Result = std::unique_ptr<const SherpaOnnxOfflineRecognizerResult,
                              decltype(&SherpaOnnxDestroyOfflineRecognizerResult)>;
using Segment = std::unique_ptr<const SherpaOnnxSpeechSegment,
                               decltype(&SherpaOnnxDestroySpeechSegment)>;

struct VadReset {
    const SherpaOnnxVoiceActivityDetector *vad;
    explicit VadReset(const SherpaOnnxVoiceActivityDetector *p) : vad(p) {
        SherpaOnnxVoiceActivityDetectorReset(vad);
    }
    ~VadReset() { SherpaOnnxVoiceActivityDetectorReset(vad); }
};

void feed_vad(const SherpaOnnxVoiceActivityDetector *vad,
              const float *samples, size_t count) {
    // Feed full windows. Zero-pad only the VAD tail, never the ASR input.
    size_t offset = 0;
    while (count - offset >= 512) {
        SherpaOnnxVoiceActivityDetectorAcceptWaveform(vad, samples + offset, 512);
        offset += 512;
    }
    if (offset < count) {
        float tail[512]{};
        std::copy(samples + offset, samples + count, tail);
        SherpaOnnxVoiceActivityDetectorAcceptWaveform(vad, tail, 512);
    }
    SherpaOnnxVoiceActivityDetectorFlush(vad);
}

std::string integrity_option(const SherpaOnnxOfflineStream *stream, const char *key) {
    if (!SherpaOnnxOfflineStreamHasOption(stream, key))
        throw std::runtime_error("Nano runtime is missing required output-integrity metadata");
    const char *value = SherpaOnnxOfflineStreamGetOption(stream, key);
    if (!value) throw std::runtime_error("Nano runtime returned null integrity metadata");
    // Copy while the stream is alive and the caller holds the recognizer Mutex.
    return std::string(value);
}
}

extern "C" VtNano *vt_nano_init(const char *directory, const char *vad_model,
                                int use_itn, char *error, size_t cap) {
    try {
        if (!directory || !vad_model) throw std::runtime_error("missing model path");
        // The pinned 11afbd00 source archive's unchanged version.cc reports
        // 8c8e275d. Exact archive/patch/library hashes, not this stale source
        // stamp alone, establish the separately built integrity runtime.
        if (std::strcmp(SherpaOnnxGetVersionStr(), "1.13.8") ||
            std::strcmp(SherpaOnnxGetGitSha1(), "8c8e275d") ||
            std::strcmp(SherpaOnnxGetOnnxruntimeVersionStr(), "1.28.2"))
            throw std::runtime_error("unvalidated sherpa/ORT runtime version");
        auto ctx = std::make_unique<VtNano>();
        std::string root(directory);
        std::string encoder = root + "/encoder_adaptor.int8.onnx";
        std::string embedding = root + "/embedding.int8.onnx";
        std::string llm = root + "/llm.int8.onnx";
        std::string tokenizer = root + "/Qwen3-0.6B";
        SherpaOnnxOfflineRecognizerConfig config{};
        config.feat_config.sample_rate = 16000;
        config.feat_config.feature_dim = 80;
        config.decoding_method = "greedy_search";
        config.model_config.num_threads = 4;
        config.model_config.provider = "cpu";
        auto &nano = config.model_config.funasr_nano;
        nano.encoder_adaptor = encoder.c_str();
        nano.embedding = embedding.c_str();
        nano.llm = llm.c_str();
        nano.tokenizer = tokenizer.c_str();
        nano.system_prompt = "You are a helpful assistant.";
        nano.user_prompt = "语音转写:";  // Explicit ASCII colon: measured Python config.
        nano.max_new_tokens = 512;
        nano.temperature = 1e-6f;
        nano.top_p = 0.8f;
        nano.seed = 42;
        nano.language = "";
        nano.itn = use_itn;
        nano.hotwords = "";
        // Validate VAD first, before allocating the large recognizer.
        SherpaOnnxVadModelConfig vad{};
        vad.silero_vad.model = vad_model;
        vad.silero_vad.threshold = 0.5f;
        vad.silero_vad.min_silence_duration = 0.1f;
        vad.silero_vad.min_speech_duration = 0.25f;
        vad.silero_vad.window_size = 512;
        vad.silero_vad.max_speech_duration = 60.0f;
        vad.sample_rate = 16000;
        vad.num_threads = 1;
        vad.provider = "cpu";
        ctx->vad = SherpaOnnxCreateVoiceActivityDetector(&vad, 61.0f);
        if (!ctx->vad) throw std::runtime_error("failed to create independent Silero VAD");
        ctx->recognizer = SherpaOnnxCreateOfflineRecognizer(&config);
        if (!ctx->recognizer) throw std::runtime_error("failed to create Nano recognizer");
        std::fprintf(stderr, "Nano native: sherpa=%s git=%s ort=%s provider=cpu threads=4 language=auto itn=%d; full-waveform speech gate\n",
            SherpaOnnxGetVersionStr(), SherpaOnnxGetGitSha1(),
            SherpaOnnxGetOnnxruntimeVersionStr(), use_itn);
        return ctx.release();
    } catch (const std::exception &e) { error_text(error, cap, e.what()); }
      catch (...) { error_text(error, cap, "unknown native initialization failure"); }
    return nullptr;
}

extern "C" void vt_nano_free(VtNano *ctx) { delete ctx; }

extern "C" int vt_nano_transcribe(VtNano *ctx, const float *samples, size_t count,
                                  char *out, size_t out_cap, char *error, size_t error_cap) {
    if (out && out_cap) out[0] = '\0';
    try {
        if (!ctx || !out || !out_cap) throw std::runtime_error("invalid Nano argument");
        validate_audio(samples, count);
        if (!count) return 0;
        Stream stream(SherpaOnnxCreateOfflineStream(ctx->recognizer), SherpaOnnxDestroyOfflineStream);
        if (!stream) throw std::runtime_error("failed to create Nano stream");
        SherpaOnnxAcceptWaveformOffline(stream.get(), 16000, samples, static_cast<int32_t>(count));
        SherpaOnnxDecodeOfflineStream(ctx->recognizer, stream.get());
        // Only the patched decoder supplies this evidence. Never seed stream
        // options here or infer completion from punctuation/text length.
        const auto version = integrity_option(stream.get(), "voicetype.nano.integrity_version");
        const auto reason = integrity_option(stream.get(), "voicetype.nano.stop_reason");
        // Diagnostic contract evidence only; never log transcript/audio here.
        std::fprintf(stderr, "Nano integrity: version=%s stop_reason=%s\n",
                     version.c_str(), reason.c_str());
        if (version != "1") throw std::runtime_error("unsupported Nano integrity metadata version");
        if (reason != "eos" && reason != "no_speech")
            throw std::runtime_error("Nano did not finish safely (" + reason + "); partial text withheld");
        Result result(SherpaOnnxGetOfflineStreamResult(stream.get()), SherpaOnnxDestroyOfflineRecognizerResult);
        if (!result || !result->text) throw std::runtime_error("missing Nano result");
        if (reason == "no_speech" && result->text[0] != '\0')
            throw std::runtime_error("Nano no_speech status contains text; result withheld");
        size_t length = std::strlen(result->text);
        if (length >= out_cap) throw std::runtime_error("Nano output exceeds byte limit");
        std::memcpy(out, result->text, length + 1);
        return static_cast<int>(length);
    } catch (const std::exception &e) { error_text(error, error_cap, e.what()); }
      catch (...) { error_text(error, error_cap, "unknown native decode failure"); }
    return -1;
}

extern "C" int vt_nano_has_speech(VtNano *ctx, const float *samples, size_t count,
                                 char *error, size_t error_cap) {
    try {
        if (!ctx) throw std::runtime_error("missing Nano context");
        validate_audio(samples, count);
        VadReset reset(ctx->vad);
        if (!count) return 0;
        feed_vad(ctx->vad, samples, count);
        const bool speech = !SherpaOnnxVoiceActivityDetectorEmpty(ctx->vad);
        return speech ? 1 : 0;
    } catch (const std::exception &e) { error_text(error, error_cap, e.what()); }
      catch (...) { error_text(error, error_cap, "unknown VAD failure"); }
    return -1;
}

extern "C" int vt_nano_speech_spans(VtNano *ctx, const float *samples, size_t count,
                                    VtNanoSpeechSpan *spans, size_t spans_cap,
                                    char *error, size_t error_cap) {
    try {
        if (!ctx || !spans || !spans_cap)
            throw std::runtime_error("invalid Nano speech-span argument");
        validate_audio(samples, count);
        VadReset reset(ctx->vad);
        if (!count) return 0;
        feed_vad(ctx->vad, samples, count);
        size_t written = 0, previous_end = 0;
        const size_t padded_count = ((count + 511) / 512) * 512;
        while (!SherpaOnnxVoiceActivityDetectorEmpty(ctx->vad)) {
            Segment segment(SherpaOnnxVoiceActivityDetectorFront(ctx->vad),
                            SherpaOnnxDestroySpeechSegment);
            if (!segment || segment->start < 0 || segment->n <= 0)
                throw std::runtime_error("invalid Nano VAD speech span");
            const size_t start = static_cast<size_t>(segment->start);
            const size_t end = start + static_cast<size_t>(segment->n);
            if (start < previous_end || start >= count || end > padded_count)
                throw std::runtime_error("Nano VAD speech span exceeds original sample timeline");
            if (written >= spans_cap || written >= 512)
                throw std::runtime_error("Nano VAD speech spans exceed capacity");
            spans[written++] = {start, std::min(end, count)};
            previous_end = end;
            SherpaOnnxVoiceActivityDetectorPop(ctx->vad);
        }
        return static_cast<int>(written);
    } catch (const std::exception &e) { error_text(error, error_cap, e.what()); }
      catch (...) { error_text(error, error_cap, "unknown VAD span failure"); }
    return -1;
}
