#include "sensevoice_shim.h"

#include "common.h"
#include "sense-voice.h"
#include "silero-vad.h"

#include <cstring>
#include <string>
#include <vector>

struct vt_sv_context {
    sense_voice_context *ctx = nullptr;
    int n_threads = 4;
    bool vad_ready = false;
    std::string last_error;
};

namespace {

/* Silero VAD 的窗形狀。上游 main.cc 用的就是這三個數, 這裡不改 ——
 * 網路的 STFT 前端假設固定長度輸入, 換一個數字出來的機率沒有意義。
 *
 * 每步前進 512 個樣本, 但網路看到 576 個: 前 64 個是上一窗的尾巴
 * (Silero 的 context), 讓窗邊界上的語音起點不被切斷。剩下的 64 個
 * 位置是反射 padding, 湊滿 conv 前端要的 640。
 */
constexpr int VAD_HOP = VT_SV_VAD_HOP; /* 512 */
constexpr int VAD_CONTEXT = 576;
constexpr int VAD_TENSOR_LEN = 640;

/* 與上游 main.cc / stream.cc 一致。LSTM 只有 128 維隱狀態, 2KB 的
 * metadata arena 綽綽有餘 (只放兩個 tensor 的描述, 資料在 backend buffer)。 */
constexpr size_t VAD_LSTM_STATE_MEMORY_SIZE = 2048;
constexpr int64_t VAD_LSTM_STATE_DIM = 128;

/* 配置 VAD 的 LSTM 狀態張量。
 *
 * 上游把這段散在 main.cc 與 stream.cc 各抄一份 —— 不是公開 API 的一部分,
 * 但 `silero_vad_encode_internal()` 直接讀寫 `state->vad_lstm_*`, 少了這步
 * 會解參考到未初始化的指標。
 *
 * 釋放由 `sense_voice_free_state()` 負責 (它會 ggml_free vad_ctx 並
 * free 兩個 buffer), 所以這裡只配置。
 */
bool init_vad_state(sense_voice_context *ctx) {
    auto *state = ctx->state;
    if (!state || state->backends.empty()) {
        return false;
    }
    ggml_backend_t backend = state->backends[0];

    state->vad_ctx = ggml_init({VAD_LSTM_STATE_MEMORY_SIZE, nullptr, true});
    if (!state->vad_ctx) {
        return false;
    }
    state->vad_lstm_context =
        ggml_new_tensor_1d(state->vad_ctx, GGML_TYPE_F32, VAD_LSTM_STATE_DIM);
    state->vad_lstm_hidden_state =
        ggml_new_tensor_1d(state->vad_ctx, GGML_TYPE_F32, VAD_LSTM_STATE_DIM);
    if (!state->vad_lstm_context || !state->vad_lstm_hidden_state) {
        return false;
    }

    const size_t align = ggml_backend_get_alignment(backend);
    state->vad_lstm_context_buffer = ggml_backend_alloc_buffer(
        backend, ggml_nbytes(state->vad_lstm_context) + align);
    state->vad_lstm_hidden_state_buffer = ggml_backend_alloc_buffer(
        backend, ggml_nbytes(state->vad_lstm_hidden_state) + align);
    if (!state->vad_lstm_context_buffer || !state->vad_lstm_hidden_state_buffer) {
        return false;
    }

    auto ctx_alloc = ggml_tallocr_new(state->vad_lstm_context_buffer);
    ggml_tallocr_alloc(&ctx_alloc, state->vad_lstm_context);
    auto hidden_alloc = ggml_tallocr_new(state->vad_lstm_hidden_state_buffer);
    ggml_tallocr_alloc(&hidden_alloc, state->vad_lstm_hidden_state);

    ggml_set_zero(state->vad_lstm_context);
    ggml_set_zero(state->vad_lstm_hidden_state);
    return true;
}

/* 複製自 sense_voice_print_output() 的 CTC 去重邏輯, 但寫進字串而非
 * printf 到 stdout。
 *
 * `need_prefix = true`: 保留開頭那幾個標籤 token。它們是
 * `<|zh|><|NEUTRAL|><|Speech|><|withitn|>`, Rust 端的 strip_tags 需要
 * 語言標籤來判斷 per-app profile (SDD §4.5), 在這裡丟掉就拿不回來了。
 */
std::string collect_output(sense_voice_context *ctx) {
    std::string text;
    const auto &ids = ctx->state->ids;
    text.reserve(ids.size() * 3);

    for (size_t i = 0; i < ids.size(); i++) {
        int id = ids[i];
        // CTC 去重: 連續相同的 token 只留一個。
        if (i > 0 && ids[i - 1] == ids[i]) {
            continue;
        }
        if (id) {
            text += ctx->vocab.id_to_token[id];
        }
    }
    return text;
}

} // namespace

vt_sv_context *vt_sv_init(const char *model_path, int n_threads, int use_itn) {
    if (!model_path || n_threads <= 0) {
        return nullptr;
    }

    auto *wrapper = new vt_sv_context();
    wrapper->n_threads = n_threads;

    sense_voice_context_params cparams = sense_voice_context_default_params();
    // SDD §2/C2: ASR 完全跑在 CPU 上。GPU 要留給 Isaac Sim 與訓練工作,
    // 這裡不是效能取捨而是硬性約束。
    cparams.use_gpu = false;
    cparams.use_itn = use_itn != 0;

    wrapper->ctx = sense_voice_small_init_from_file_with_params(model_path, cparams);
    if (!wrapper->ctx) {
        delete wrapper;
        return nullptr;
    }

    // VAD 配置失敗不算致命: 沒有 VAD 的聽寫仍然可用 (M0 就是這樣跑的),
    // 只是失去空錄音防護。呼叫端用 vt_sv_vad_probs 的錯誤碼發現這件事。
    wrapper->vad_ready = init_vad_state(wrapper->ctx);
    return wrapper;
}

void vt_sv_free(vt_sv_context *wrapper) {
    if (!wrapper) {
        return;
    }
    if (wrapper->ctx) {
        if (wrapper->ctx->state) {
            sense_voice_free_state(wrapper->ctx->state);
            wrapper->ctx->state = nullptr;
        }
        delete wrapper->ctx;
    }
    delete wrapper;
}

int vt_sv_transcribe(vt_sv_context *wrapper, const float *samples,
                     size_t n_samples, const char *language, char *out,
                     size_t out_cap) {
    if (!wrapper || !wrapper->ctx || !samples || !out || out_cap == 0) {
        return VT_SV_ERR_INVALID_ARG;
    }
    if (n_samples == 0) {
        out[0] = '\0';
        return 0;
    }

    // 引擎接的是 double —— 音訊管線全程用 f32 (SDD §4.4), 所以這裡轉一次。
    // 一次 60 秒的錄音是 960k 個 sample, 轉換成本遠低於 250ms 的推論。
    std::vector<double> pcm(n_samples);
    for (size_t i = 0; i < n_samples; ++i) {
        pcm[i] = static_cast<double>(samples[i]);
    }

    sense_voice_full_params params =
        sense_voice_full_default_params(SENSE_VOICE_SAMPLING_GREEDY);
    params.n_threads = wrapper->n_threads;
    params.language = (language && *language) ? language : "auto";
    params.no_timestamps = true;
    params.print_progress = false;
    params.print_timestamps = false;
    params.debug_mode = false;

    // n_processors = 1: 音訊短 (PTT 上限 60 秒), 切分成多個 processor
    // 的協調成本不划算, 而且 SDD §6.2 要求限制執行緒數以免與
    // `make -j$(nproc)` 互搶。平行度由 n_threads 控制。
    int rc = sense_voice_full_parallel(wrapper->ctx, params, pcm,
                                       static_cast<int>(pcm.size()), 1);
    if (rc != 0) {
        wrapper->last_error = "sense_voice_full_parallel failed";
        return VT_SV_ERR_INFERENCE;
    }

    std::string text = collect_output(wrapper->ctx);
    if (text.size() + 1 > out_cap) {
        wrapper->last_error = "output buffer too small";
        return VT_SV_ERR_BUFFER_TOO_SMALL;
    }
    std::memcpy(out, text.data(), text.size());
    out[text.size()] = '\0';
    return static_cast<int>(text.size());
}

int vt_sv_vad_probs(vt_sv_context *wrapper, const float *samples,
                    size_t n_samples, float *out_probs, size_t out_cap) {
    if (!wrapper || !wrapper->ctx || !samples || !out_probs) {
        return VT_SV_ERR_INVALID_ARG;
    }
    if (!wrapper->vad_ready) {
        wrapper->last_error = "VAD state was not initialised at load time";
        return VT_SV_ERR_INFERENCE;
    }
    if (n_samples == 0) {
        return 0;
    }

    const size_t n_windows = (n_samples + VAD_HOP - 1) / VAD_HOP;
    if (n_windows > out_cap) {
        wrapper->last_error = "probability buffer too small";
        return VT_SV_ERR_BUFFER_TOO_SMALL;
    }

    // LSTM 狀態必須每段錄音歸零。不歸零的話, 上一次錄音結尾的
    // 「正在說話」會讓這一次的開頭偏向語音 —— 誤觸熱鍵剛好接在
    // 一次正常聽寫之後, 而那正是要防的情境。
    auto *state = wrapper->ctx->state;
    ggml_set_zero(state->vad_lstm_context);
    ggml_set_zero(state->vad_lstm_hidden_state);

    std::vector<float> chunk(VAD_TENSOR_LEN, 0.0f);

    for (size_t w = 0; w < n_windows; ++w) {
        const ptrdiff_t base = static_cast<ptrdiff_t>(w * VAD_HOP);
        // 網路看到 [base-64, base+512)。窗左邊界外 (第一窗) 與音訊結尾
        // 之後補零。
        for (int k = 0; k < VAD_CONTEXT; ++k) {
            const ptrdiff_t j = base - (VAD_CONTEXT - VAD_HOP) + k;
            chunk[k] = (j >= 0 && static_cast<size_t>(j) < n_samples)
                           ? samples[j]
                           : 0.0f;
        }
        // 反射 padding 到 640, 與上游 main.cc 的寫法一致。
        for (int k = VAD_CONTEXT; k < VAD_TENSOR_LEN; ++k) {
            chunk[k] = chunk[2 * VAD_CONTEXT - k - 2];
        }

        float prob = 0.0f;
        if (!silero_vad_encode_internal(*wrapper->ctx, *state, chunk,
                                        wrapper->n_threads, prob)) {
            wrapper->last_error = "silero_vad_encode_internal failed";
            return VT_SV_ERR_INFERENCE;
        }
        out_probs[w] = prob;
    }

    return static_cast<int>(n_windows);
}

const char *vt_sv_last_error(const vt_sv_context *wrapper) {
    if (!wrapper) {
        return "null context";
    }
    return wrapper->last_error.empty() ? "" : wrapper->last_error.c_str();
}
