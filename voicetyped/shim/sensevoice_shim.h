/* SenseVoice.cpp 的 C 介面 shim。
 *
 * 為什麼需要這一層 (見 docs/sdd-deviations.md D8): SenseVoice.cpp 的
 * 公開介面是 C++ 而非 C —— `sense_voice_full_parallel()` 接
 * `std::vector<double>&` 且帶預設參數, 沒有 `extern "C"`, Rust 無法
 * 直接 bindgen。
 *
 * 而且它**沒有回傳文字的 API**: 結果留在 `ctx->state->ids` (token id
 * 序列), 只能透過 `sense_voice_print_output()` 印到 stdout。這裡複製
 * 那段 CTC 去重邏輯, 改成寫進呼叫端的緩衝區。
 *
 * 不走子行程方案的理由: 每次錄音重啟 CLI 要重新載入 291MB 模型, 與
 * SDD §1.3 的 P50 < 400ms 目標直接衝突。
 */

#ifndef VOICETYPE_SENSEVOICE_SHIM_H
#define VOICETYPE_SENSEVOICE_SHIM_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct vt_sv_context vt_sv_context;

/* 錯誤碼。負值, 對應 SDD §4.3 的 IPC 錯誤碼。 */
#define VT_SV_ERR_INVALID_ARG (-1)
#define VT_SV_ERR_INFERENCE (-2)
#define VT_SV_ERR_BUFFER_TOO_SMALL (-3)

/* 載入模型。失敗回傳 NULL。
 *
 * use_itn: 反向文字正規化 (數字寫成阿拉伯數字等)。
 */
vt_sv_context *vt_sv_init(const char *model_path, int n_threads, int use_itn);

void vt_sv_free(vt_sv_context *ctx);

/* 轉錄 16kHz mono f32。
 *
 * 輸出是引擎的**原始**文字, 含 `<|zh|><|NEUTRAL|>` 這類結構化標籤 ——
 * 標籤剝除由 Rust 端的 postproc::strip_tags 負責 (SDD §4.6 ①), 因為
 * 語言標籤是 per-app profile 的判斷依據, 不能在這裡丟掉。
 *
 * 回傳寫入 out 的位元組數 (不含結尾 NUL), 或負的錯誤碼。
 */
int vt_sv_transcribe(vt_sv_context *ctx, const float *samples, size_t n_samples,
                     const char *language, char *out, size_t out_cap);

/* ── Silero VAD (SDD §4.4) ────────────────────────────────────────────
 *
 * VAD 權重就在 SenseVoice 的 GGUF 裡 (`_model.stft.*` / `_model.encoder.*`),
 * 不需要額外的模型檔, 也不需要第二份 backend —— 共用 `vt_sv_init` 建好的
 * context。
 *
 * 這裡只回傳**每個窗的語音機率**, 不做分段。門檻、遲滯與前後留白屬於
 * 策略, 放在 Rust 端才測得到 (見 voicetyped/src/vad/mod.rs) ——
 * shim 是 unsafe 邊界, 邊界上的程式碼愈少愈好。
 */

/* 每個窗前進的樣本數。16kHz 下是 32ms。 */
#define VT_SV_VAD_HOP 512

/* 掃描整段音訊, 每 VT_SV_VAD_HOP 個樣本產生一個 [0,1] 的語音機率。
 *
 * samples: 16kHz mono f32, 已正規化到 [-1, 1]。
 * out_probs: 需能容納 ceil(n_samples / VT_SV_VAD_HOP) 個 float。
 *
 * 回傳寫入的窗數, 或負的錯誤碼。
 */
int vt_sv_vad_probs(vt_sv_context *ctx, const float *samples, size_t n_samples,
                    float *out_probs, size_t out_cap);

/* 最後一次錯誤的說明。永遠回傳可用的 C 字串。 */
const char *vt_sv_last_error(const vt_sv_context *ctx);

#ifdef __cplusplus
}
#endif

#endif
