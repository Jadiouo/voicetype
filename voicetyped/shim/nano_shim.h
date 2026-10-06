#pragma once
#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif
typedef struct VtNano VtNano;
typedef struct VtNanoSpeechSpan {
    size_t start;
    size_t end;
} VtNanoSpeechSpan;
VtNano *vt_nano_init(const char *model_dir, const char *vad_model, int use_itn,
                     char *error, size_t error_cap);
void vt_nano_free(VtNano *ctx);
int vt_nano_transcribe(VtNano *ctx, const float *samples, size_t count,
                       char *out, size_t out_cap, char *error, size_t error_cap);
int vt_nano_has_speech(VtNano *ctx, const float *samples, size_t count,
                      char *error, size_t error_cap);
// Original input sample coordinates, with only VAD-only tail padding clamped.
// Returns span count or -1; callers must discard all spans on error.
int vt_nano_speech_spans(VtNano *ctx, const float *samples, size_t count,
                         VtNanoSpeechSpan *spans, size_t spans_cap,
                         char *error, size_t error_cap);
#ifdef __cplusplus
}
#endif
