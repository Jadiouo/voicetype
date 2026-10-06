// Deterministic C-ABI contract test: calls the real shim against a fake runtime.
// No models, recordings, GPU, or generated answers are involved.
#include "nano_shim.h"
#include "sherpa-onnx/c-api/c-api.h"
#include <cassert>
#include <limits>
#include <cstring>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

struct Scenario {
    bool version_present = true;
    bool reason_present = true;
    std::string version = "1";
    std::string reason = "eos";
    std::string text = "complete text";
    bool throws = false;
    bool null_version = false, null_reason = false, null_result = false, null_text = false;
    bool null_stream = false, accept_throws = false, option_throws = false, result_throws = false;
    size_t output_capacity = 128;
};
static Scenario next;
static int created_streams = 0, destroyed_streams = 0;
static int created_results = 0, destroyed_results = 0;
static int created_recognizers = 0, destroyed_recognizers = 0;
static int created_vads = 0, destroyed_vads = 0;
static int created_segments = 0, destroyed_segments = 0, vad_resets = 0;
static std::vector<std::pair<int32_t, int32_t>> vad_segments;
static size_t vad_index = 0;
static std::vector<float> vad_fed;
static bool vad_null_front = false, vad_throw_front = false;
struct SherpaOnnxOfflineRecognizer {};
struct SherpaOnnxVoiceActivityDetector {};
struct SherpaOnnxOfflineStream { Scenario scenario; };
extern "C" {
const char *SherpaOnnxGetVersionStr() { return "1.13.8"; }
const char *SherpaOnnxGetGitSha1() { return "8c8e275d"; }
const char *SherpaOnnxGetOnnxruntimeVersionStr() { return "1.28.2"; }
const SherpaOnnxOfflineRecognizer *SherpaOnnxCreateOfflineRecognizer(const SherpaOnnxOfflineRecognizerConfig *c) {
    assert(c && std::string(c->model_config.provider) == "cpu");
    assert(c->model_config.num_threads == 4);
    assert(std::string(c->model_config.funasr_nano.user_prompt) == "语音转写:");
    ++created_recognizers;
    return new SherpaOnnxOfflineRecognizer;
}
void SherpaOnnxDestroyOfflineRecognizer(const SherpaOnnxOfflineRecognizer *p) { ++destroyed_recognizers; delete p; }
const SherpaOnnxVoiceActivityDetector *SherpaOnnxCreateVoiceActivityDetector(const SherpaOnnxVadModelConfig *c, float) {
    assert(std::string(c->provider) == "cpu"); ++created_vads; return new SherpaOnnxVoiceActivityDetector;
}
void SherpaOnnxDestroyVoiceActivityDetector(const SherpaOnnxVoiceActivityDetector *p) { ++destroyed_vads; delete p; }
void SherpaOnnxVoiceActivityDetectorReset(const SherpaOnnxVoiceActivityDetector *) { ++vad_resets; vad_index = 0; }
void SherpaOnnxVoiceActivityDetectorAcceptWaveform(const SherpaOnnxVoiceActivityDetector *, const float *samples, int32_t n) {
    assert(n == 512); vad_fed.insert(vad_fed.end(), samples, samples + n);
}
void SherpaOnnxVoiceActivityDetectorFlush(const SherpaOnnxVoiceActivityDetector *) {}
int32_t SherpaOnnxVoiceActivityDetectorEmpty(const SherpaOnnxVoiceActivityDetector *) { return vad_index == vad_segments.size(); }
const SherpaOnnxSpeechSegment *SherpaOnnxVoiceActivityDetectorFront(const SherpaOnnxVoiceActivityDetector *) {
    if (vad_throw_front) throw std::runtime_error("controlled VAD front failure");
    if (vad_null_front) return nullptr;
    auto [start, n] = vad_segments.at(vad_index);
    ++created_segments;
    return new SherpaOnnxSpeechSegment{start, nullptr, n};
}
void SherpaOnnxVoiceActivityDetectorPop(const SherpaOnnxVoiceActivityDetector *) { ++vad_index; }
void SherpaOnnxDestroySpeechSegment(const SherpaOnnxSpeechSegment *s) { ++destroyed_segments; delete s; }
const SherpaOnnxOfflineStream *SherpaOnnxCreateOfflineStream(const SherpaOnnxOfflineRecognizer *) {
    if (next.null_stream) return nullptr;
    ++created_streams; return new SherpaOnnxOfflineStream{next};
}
void SherpaOnnxDestroyOfflineStream(const SherpaOnnxOfflineStream *s) { ++destroyed_streams; delete s; }
void SherpaOnnxAcceptWaveformOffline(const SherpaOnnxOfflineStream *s, int32_t rate, const float *, int32_t) { assert(rate == 16000); if (s->scenario.accept_throws) throw std::runtime_error("controlled accept failure"); }
void SherpaOnnxDecodeOfflineStream(const SherpaOnnxOfflineRecognizer *, const SherpaOnnxOfflineStream *s) {
    if (s->scenario.throws) throw std::runtime_error("controlled native decode failure");
}
int32_t SherpaOnnxOfflineStreamHasOption(const SherpaOnnxOfflineStream *s, const char *key) {
    if (std::string(key) == "voicetype.nano.integrity_version") return s->scenario.version_present;
    if (std::string(key) == "voicetype.nano.stop_reason") return s->scenario.reason_present;
    assert(false && "unrecognized option key"); return 0;
}
const char *SherpaOnnxOfflineStreamGetOption(const SherpaOnnxOfflineStream *s, const char *key) {
    if (s->scenario.option_throws) throw std::runtime_error("controlled option failure");
    if (std::string(key) == "voicetype.nano.integrity_version") return s->scenario.null_version ? nullptr : s->scenario.version.c_str();
    if (std::string(key) == "voicetype.nano.stop_reason") return s->scenario.null_reason ? nullptr : s->scenario.reason.c_str();
    assert(false && "unrecognized option key"); return "";
}
void SherpaOnnxOfflineStreamSetOption(const SherpaOnnxOfflineStream *, const char *, const char *) {
    assert(false && "the shim must not manufacture integrity metadata");
}
const SherpaOnnxOfflineRecognizerResult *SherpaOnnxGetOfflineStreamResult(const SherpaOnnxOfflineStream *s) {
    if (s->scenario.result_throws) throw std::runtime_error("controlled result failure");
    if (s->scenario.null_result) return nullptr;
    auto *r = new SherpaOnnxOfflineRecognizerResult{};
    if (s->scenario.null_text) { ++created_results; return r; }
    char *copy = new char[s->scenario.text.size()+1];
    std::memcpy(copy, s->scenario.text.c_str(), s->scenario.text.size()+1);
    r->text = copy; ++created_results; return r;
}
void SherpaOnnxDestroyOfflineRecognizerResult(const SherpaOnnxOfflineRecognizerResult *r) {
    ++destroyed_results; delete[] r->text; delete r;
}
}

static void expect(VtNano *ctx, Scenario scenario, bool accepted) {
    next = scenario;
    char output[128] = "stale output from previous call";
    char error[256]{};
    float samples[] = {0.2f, -0.1f};
    const int result = vt_nano_transcribe(ctx, samples, 2, output, scenario.output_capacity, error, sizeof(error));
    if (accepted) {
        assert(result == static_cast<int>(scenario.text.size()));
        assert(std::string(output) == scenario.text);
    } else {
        if (result >= 0) {
            std::cerr << "incorrect success: version_present=" << scenario.version_present
                      << " version=" << scenario.version << " reason=" << scenario.reason << "\n";
        }
        assert(result < 0);
        assert(output[0] == '\0');
        assert(error[0] != '\0');
    }
    assert(created_streams == destroyed_streams);
    assert(created_results == destroyed_results);
}

static void span_contract(VtNano *ctx) {
    const std::vector<float> audio(777, 0.25f);
    VtNanoSpeechSpan spans[4]{};
    char error[256]{};
    auto call = [&](size_t capacity) {
        vad_fed.clear();
        const int before = vad_resets;
        const int rc = vt_nano_speech_spans(ctx, audio.data(), audio.size(), spans,
                                           capacity, error, sizeof(error));
        assert(vad_resets == before + 2); // Reset even after metadata errors.
        assert(created_segments == destroyed_segments);
        assert(vad_fed.size() == 1024);
        for (size_t i = 0; i < vad_fed.size(); ++i)
            assert(vad_fed[i] == (i < audio.size() ? 0.25f : 0.0f));
        return rc;
    };
    vad_segments = {{0, 100}, {200, 824}};
    assert(call(4) == 2);
    assert(spans[0].start == 0 && spans[0].end == 100);
    assert(spans[1].start == 200 && spans[1].end == 777);
    assert(call(1) < 0); // Partial metadata must not report success.
    for (const auto &bad : std::vector<std::vector<std::pair<int32_t,int32_t>>>{
        {{-1, 100}}, {{0, 0}}, {{0, -1}}, {{777, 1}}, {{0, 1025}},
        {{0, 300}, {200, 100}}, {{300, 100}, {0, 100}}}) {
        vad_segments = bad;
        assert(call(4) < 0);
    }
    vad_segments = {{0, 100}};
    vad_null_front = true; assert(call(4) < 0); vad_null_front = false;
    vad_throw_front = true; assert(call(4) < 0); vad_throw_front = false;
    assert(call(4) == 1); // Fresh VAD still works after failures.
    vad_segments.clear(); assert(call(4) == 0);
    assert(vt_nano_speech_spans(ctx, nullptr, 0, spans, 4, error, sizeof(error)) == 0);
    assert(vt_nano_speech_spans(ctx, audio.data(), audio.size(), nullptr, 4, error, sizeof(error)) < 0);
    assert(vt_nano_speech_spans(ctx, audio.data(), audio.size(), spans, 0, error, sizeof(error)) < 0);
    std::cout << "speech span boundary/capacity/tail-padding/reset/RAII controls passed\n";
}

int main() {
    char error[256]{};
    VtNano *ctx = vt_nano_init("/controlled/model", "/controlled/vad", 1, error, sizeof(error));
    assert(ctx);
    span_contract(ctx);
    Scenario stock; stock.version_present = stock.reason_present = false;
    expect(ctx, stock, false);
    expect(ctx, Scenario{}, true);
    // A fresh stock/unknown stream must not borrow the previous stream's EOS.
    expect(ctx, stock, false);
    Scenario missing_reason; missing_reason.reason_present = false;
    expect(ctx, missing_reason, false);
    for (const char *version : {"", "0", "2", "1-extra"}) {
        Scenario s; s.version = version; expect(ctx, s, false);
    }
    for (const char *reason : {"", "unknown", "context_limit", "max_new_tokens", "audio_truncated", "input_too_short", "invalid_model_output", "im_end", "future_status"}) {
        Scenario s; s.reason = reason; expect(ctx, s, false);
        s.text.clear(); expect(ctx, s, false); // A partial error is not ordinary silence.
    }
    Scenario no_speech; no_speech.reason = "no_speech"; no_speech.text.clear();
    expect(ctx, no_speech, true);
    no_speech.text = "hallucination"; expect(ctx, no_speech, false);
    Scenario empty_eos; empty_eos.text.clear(); expect(ctx, empty_eos, true);
    Scenario throwing; throwing.throws = true; expect(ctx, throwing, false);
    expect(ctx, Scenario{}, true); // Subsequent healthy stream remains independent.
    for (const char *reason : {"eos", "no_speech"}) {
        Scenario s; s.reason = reason; s.text.clear();
        s.null_result = true; expect(ctx, s, false);
        s.null_result = false; s.null_text = true; expect(ctx, s, false);
    }
    Scenario nv; nv.null_version = true; expect(ctx, nv, false);
    Scenario nr; nr.null_reason = true; expect(ctx, nr, false);
    Scenario ns; ns.null_stream = true; expect(ctx, ns, false);
    Scenario at; at.accept_throws = true; expect(ctx, at, false);
    Scenario ot; ot.option_throws = true; expect(ctx, ot, false);
    Scenario rt; rt.result_throws = true; expect(ctx, rt, false);
    Scenario exact; exact.text.assign(127, 'x'); expect(ctx, exact, true);
    exact.text.push_back('x'); expect(ctx, exact, false);
    Scenario one; one.output_capacity = 1; one.text.clear(); expect(ctx, one, true);
    one.text = "x"; expect(ctx, one, false);
    expect(ctx, Scenario{}, true);
    // Audio/argument failures must clear an already populated caller buffer too.
    for (int kind=0; kind<4; ++kind) {
        char output[128] = "old accepted text";
        float bad[] = {std::numeric_limits<float>::quiet_NaN()};
        const int before = created_streams;
        int rc = vt_nano_transcribe(kind==0 ? nullptr : ctx, kind==1 ? nullptr : bad,
            kind==2 ? 960001 : 1, output, sizeof(output), error, sizeof(error));
        assert(rc < 0 && output[0] == '\0' && created_streams == before);
    }
    { // Local zero-audio shortcut is not runtime completeness evidence.
        next = stock;
        char output[8] = "stale";
        const int before = created_streams;
        assert(vt_nano_transcribe(ctx, nullptr, 0, output, sizeof(output), error, sizeof(error)) == 0);
        assert(output[0]=='\0' && created_streams==before);
    }
    expect(ctx, Scenario{}, true);
    std::cout << "independent null/exception/capacity/argument/stale-output controls passed\n";
    vt_nano_free(ctx);
    assert(created_recognizers == destroyed_recognizers && created_recognizers == 1);
    assert(created_vads == destroyed_vads && created_vads == 1);
    std::cout << "integrity contract cases passed; all stream/result/model handles destroyed\n";
}
