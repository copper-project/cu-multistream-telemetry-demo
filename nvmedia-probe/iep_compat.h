/* Minimal ABI declarations for the standalone DRIVE IEP 1.1.0 experiment.
 * Independently expressed from the public DRIVE OS 7.0.3 API reference.
 * See README.md for sources. This is not an NVIDIA SDK replacement.
 */
#ifndef CU_IEP_COMPAT_H
#define CU_IEP_COMPAT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef struct IepOpaque *Iep;
typedef struct BufModuleOpaque *BufModule;
typedef struct BufAttrsOpaque *BufAttrs;
typedef struct BufOpaque *Buf;
typedef struct SyncModuleOpaque *SyncModule;
typedef struct SyncAttrsOpaque *SyncAttrs;
typedef struct SyncOpaque *Sync;
typedef struct WaitContextOpaque *WaitContext;

typedef struct {
    uint8_t major, minor, patch;
} IepVersion;
typedef struct {
    uint16_t width, height;
} Resolution;
typedef struct {
    uint32_t key;
    const void *value;
    size_t len;
} Attr;
typedef struct {
    uint64_t payload[6];
} Fence;
typedef struct {
    uint8_t *data;
    uint32_t used, capacity;
} Bitstream;
typedef struct {
    int16_t p, b, i, reserved[3];
} Qp;
typedef struct {
    int mode;
    uint32_t b_frames;
    union {
        struct {
            uint32_t bitrate, vbv_size, vbv_delay;
        } cbr;
        Qp constant;
        struct {
            uint32_t bitrate, max_bitrate, vbv_size, vbv_delay;
        } vbr;
        struct {
            uint32_t bitrate, max_bitrate, vbv_size, vbv_delay;
            Qp min;
        } vbr_min;
        struct {
            uint32_t bitrate, vbv_size, vbv_delay;
            Qp min;
        } cbr_min;
    } params;
    bool constant_frame_qp;
    int8_t max_session_qp, reserved[3];
} RateControl;
typedef struct {
    uint32_t block16x16, block16x8, block8x16, block8x8;
} MeCounts;
typedef struct {
    uint16_t width, reserved1, height, reserved2;
    bool limited_rgb, slice_level;
    uint32_t fps_num, fps_den;
    uint8_t profile, level, tier, max_refs;
    bool external_me;
    MeCounts max_me[2];
    bool recon_crc, mvc, roi, slice;
    uint32_t b_frames_as_refs;
    bool two_pass, half_scaled;
    uint32_t views : 4;
    uint32_t external_rc : 1;
    uint32_t all_i : 1;
    uint32_t memory_optimization : 1;
    uint32_t disable_memory_optimization : 1;
    uint32_t reserved : 24;
    bool amp_disable;
} InitH265;
typedef struct {
    uint32_t features, gop;
    RateControl rc;
    int repeat_headers;
    uint32_t idr_period;
    uint16_t slices_minus_one;
    uint8_t disable_deblock, weighted_prediction;
    uint32_t intra_refresh_period, intra_refresh_count, max_slice_bytes, ctus_per_slice;
    void *vui;
    Qp initial_qp, max_qp;
    int preset;
} ConfigH265;
typedef struct {
    int type;
    uint32_t flags, next_b_frames;
    RateControl rc;
    uint32_t sei_count;
    void *sei;
    MeCounts me_counts[2];
    void *me_hints;
    uint32_t fps_num, fps_den, view;
} PictureH265;

enum {
    BUF_TYPES = 1,
    BUF_CPU_ACCESS = 2,
    BUF_PERMISSION = 3,
    BUF_CPU_CACHE = 4,
    BUF_CACHE_COHERENCY = 6,
    IMAGE_LAYOUT = 0x20000,
    IMAGE_SIZE = 0x20006,
    IMAGE_PLANES = 0x20008,
    IMAGE_FORMAT = 0x20009,
    IMAGE_COLOR = 0x2000a,
    IMAGE_PLANE_ALIGN = 0x2000b,
    IMAGE_WIDTH = 0x2000c,
    IMAGE_HEIGHT = 0x2000d,
    IMAGE_SCAN = 0x2000e,
    IMAGE_COUNT = 0x20017,
    SURFACE_TYPE = 0x20018,
    SURFACE_MEMORY = 0x20019,
    SURFACE_SAMPLE = 0x2001a,
    SURFACE_BPC = 0x2001b,
    SURFACE_ORDER = 0x2001c,
    SURFACE_WIDTH = 0x2001d,
    SURFACE_HEIGHT = 0x2001e,
    SURFACE_COLOR = 0x2001f,
    SYNC_CPU_ACCESS = 1,
    SYNC_PERMISSION = 2,
    CODEC_HEVC = 1,
    PROFILE_REXT = 4,
    FEATURE_AUD = 1,
    FEATURE_LOSSLESS = 1 << 4,
    PIC_IDR = 4,
    PIC_HEADERS = 1,
    RC_CONSTANT_QP = 1,
    IEP_SIGNALER = 0,
    IEP_WAITER = 1,
    IEP_PRE = 0,
    IEP_EOF = 1,
    COLOR_Y8 = 0x54,
    COLOR_V8U8 = 0x4c
};

/* Separate lists retain the origin of each dynamically loaded symbol. */
#define IEP_FUNCTIONS(X)                                                                           \
    X(int, NvMediaIEPGetVersion, (IepVersion *))                                                   \
    X(int, NvMediaIEPFillNvSciBufAttrList, (int, BufAttrs))                                        \
    X(Iep, NvMediaIEPCreate, (int, const void *, BufAttrs, uint8_t, int))                          \
    X(void, NvMediaIEPDestroy, (Iep))                                                              \
    X(int, NvMediaIEPSetConfiguration, (Iep, const void *))                                        \
    X(int, NvMediaIEPSetAttribute, (Iep, int, uint32_t, const void *))                             \
    X(int, NvMediaIEPRegisterNvSciBufObj, (Iep, Buf))                                              \
    X(int, NvMediaIEPUnregisterNvSciBufObj, (Iep, Buf))                                            \
    X(int, NvMediaIEPFillNvSciSyncAttrList, (Iep, SyncAttrs, int))                                 \
    X(int, NvMediaIEPRegisterNvSciSyncObj, (Iep, int, Sync))                                       \
    X(int, NvMediaIEPUnregisterNvSciSyncObj, (Iep, Sync))                                          \
    X(int, NvMediaIEPSetNvSciSyncObjforEOF, (Iep, Sync))                                           \
    X(int, NvMediaIEPInsertPreNvSciSyncFence, (Iep, const Fence *))                                \
    X(int, NvMediaIEPFeedFrame, (Iep, Buf, const void *, int))                                     \
    X(int, NvMediaIEPGetEOFNvSciSyncFence, (Iep, Sync, Fence *))                                   \
    X(int, NvMediaIEPBitsAvailable, (Iep, uint32_t *, int, uint32_t))                              \
    X(int, NvMediaIEPGetBits, (Iep, uint32_t *, uint32_t, const Bitstream *, void *))

#define BUF_FUNCTIONS(X)                                                                           \
    X(uint32_t, NvSciBufCheckVersionCompatibility, (uint32_t, uint32_t, bool *))                   \
    X(uint32_t, NvSciBufModuleOpen, (BufModule *))                                                 \
    X(void, NvSciBufModuleClose, (BufModule))                                                      \
    X(uint32_t, NvSciBufAttrListCreate, (BufModule, BufAttrs *))                                   \
    X(void, NvSciBufAttrListFree, (BufAttrs))                                                      \
    X(uint32_t, NvSciBufAttrListSetAttrs, (BufAttrs, Attr *, size_t))                              \
    X(uint32_t, NvSciBufAttrListGetAttrs, (BufAttrs, Attr *, size_t))                              \
    X(uint32_t, NvSciBufAttrListReconcile, (const BufAttrs *, size_t, BufAttrs *, BufAttrs *))     \
    X(uint32_t, NvSciBufObjAlloc, (BufAttrs, Buf *))                                               \
    X(void, NvSciBufObjFree, (Buf))                                                                \
    X(uint32_t, NvSciBufObjGetAttrList, (Buf, BufAttrs *))                                         \
    X(uint32_t, NvSciBufObjPutPixels,                                                              \
      (Buf, const void *, const void **, const uint32_t *, const uint32_t *))                      \
    X(uint32_t, NvSciBufObjGetPixels,                                                              \
      (Buf, const void *, void **, const uint32_t *, const uint32_t *))                            \
    X(uint32_t, NvSciBufObjFlushCpuCacheRange, (Buf, uint64_t, uint64_t))

#define SYNC_FUNCTIONS(X)                                                                          \
    X(uint32_t, NvSciSyncCheckVersionCompatibility, (uint32_t, uint32_t, bool *))                  \
    X(uint32_t, NvSciSyncModuleOpen, (SyncModule *))                                               \
    X(void, NvSciSyncModuleClose, (SyncModule))                                                    \
    X(uint32_t, NvSciSyncCpuWaitContextAlloc, (SyncModule, WaitContext *))                         \
    X(void, NvSciSyncCpuWaitContextFree, (WaitContext))                                            \
    X(uint32_t, NvSciSyncAttrListCreate, (SyncModule, SyncAttrs *))                                \
    X(void, NvSciSyncAttrListFree, (SyncAttrs))                                                    \
    X(uint32_t, NvSciSyncAttrListSetAttrs, (SyncAttrs, const Attr *, size_t))                      \
    X(uint32_t, NvSciSyncAttrListReconcile, (const SyncAttrs *, size_t, SyncAttrs *, SyncAttrs *)) \
    X(uint32_t, NvSciSyncObjAlloc, (SyncAttrs, Sync *))                                            \
    X(void, NvSciSyncObjFree, (Sync))                                                              \
    X(uint32_t, NvSciSyncObjGenerateFence, (Sync, Fence *))                                        \
    X(uint32_t, NvSciSyncObjSignal, (Sync))                                                        \
    X(uint32_t, NvSciSyncFenceWait, (const Fence *, WaitContext, int64_t))                         \
    X(void, NvSciSyncFenceClear, (Fence *))

/* These check the documented LP64 layouts, not compatibility with a binary. */
_Static_assert(sizeof(void *) == 8 && sizeof(int) == 4, "LP64 ABI required");
_Static_assert(sizeof(IepVersion) == 3, "version layout");
_Static_assert(sizeof(Attr) == 24 && offsetof(Attr, value) == 8, "attribute layout");
_Static_assert(sizeof(Fence) == 48, "fence layout");
_Static_assert(sizeof(Bitstream) == 16, "bitstream layout");
_Static_assert(sizeof(RateControl) == 44, "rate-control layout");
_Static_assert(offsetof(InitH265, max_me) == 28, "initialization layout");
_Static_assert(sizeof(InitH265) == 76 && offsetof(InitH265, amp_disable) == 75,
               "initialization size");
_Static_assert(sizeof(ConfigH265) == 120, "configuration size");
_Static_assert(sizeof(PictureH265) == 128, "picture size");

#endif
