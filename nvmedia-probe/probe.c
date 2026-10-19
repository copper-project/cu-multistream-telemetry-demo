#define _GNU_SOURCE
#include "iep_compat.h"

#include <dlfcn.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <unistd.h>

struct Api {
#define DECLARE(result, name, args) result(*name) args;
    IEP_FUNCTIONS(DECLARE)
    BUF_FUNCTIONS(DECLARE)
    SYNC_FUNCTIONS(DECLARE)
#undef DECLARE
};
static struct Api api;

#define CHECK(call)                                                                                \
    do {                                                                                           \
        uint32_t status_ = (uint32_t)(call);                                                       \
        printf("call=%s status=0x%08x\n", #call, status_);                                         \
        if (status_ != 0)                                                                          \
            goto cleanup;                                                                          \
    } while (0)
#define ATTR(key_, value_) {key_, &(value_), sizeof(value_)}

static int load_api(void) {
    void *iep = dlopen("libnvmedia_iep_sci.so", RTLD_NOW | RTLD_LOCAL);
    void *buf = dlopen("libnvscibuf.so", RTLD_NOW | RTLD_LOCAL);
    void *sync = dlopen("libnvscisync.so", RTLD_NOW | RTLD_LOCAL);
    if (!iep || !buf || !sync) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return -1;
    }
#define LOAD_FROM(handle, result, name, args)                                                      \
    do {                                                                                           \
        void *symbol_ = dlsym(handle, #name);                                                      \
        if (!symbol_) {                                                                            \
            fprintf(stderr, "dlsym %s: %s\n", #name, dlerror());                                   \
            return -1;                                                                             \
        }                                                                                          \
        _Static_assert(sizeof(api.name) == sizeof(symbol_), "function pointer size");              \
        memcpy(&api.name, &symbol_, sizeof(symbol_));                                              \
    } while (0);
#define LOAD_IEP(result, name, args) LOAD_FROM(iep, result, name, args)
#define LOAD_BUF(result, name, args) LOAD_FROM(buf, result, name, args)
#define LOAD_SYNC(result, name, args) LOAD_FROM(sync, result, name, args)
    IEP_FUNCTIONS(LOAD_IEP)
    BUF_FUNCTIONS(LOAD_BUF)
    SYNC_FUNCTIONS(LOAD_SYNC)
#undef LOAD_IEP
#undef LOAD_BUF
#undef LOAD_SYNC
#undef LOAD_FROM
    return 0;
}

static int mapped_libraries(void) {
    FILE *maps = fopen("/proc/self/maps", "r");
    if (!maps)
        return -1;
    char line[4096], previous[4096] = "";
    int forbidden = 0;
    while (fgets(line, sizeof(line), maps)) {
        char *path = strchr(line, '/');
        if (!path)
            continue;
        if (strstr(path, "v4l2") || strstr(path, "libgst"))
            forbidden = 1;
        if (strstr(path, ".so") && strcmp(path, previous)) {
            printf("mapped=%s", path);
            memcpy(previous, path, strlen(path) + 1);
        }
    }
    fclose(maps);
    printf("v4l2_or_gstreamer_loaded=%s\n", forbidden ? "true" : "false");
    return forbidden ? -1 : 0;
}

static int make_sync(SyncModule module, Iep iep, bool eof, Sync *out) {
    SyncAttrs engine = NULL, cpu = NULL, reconciled = NULL, conflict = NULL;
    int result = -1;
    bool cpu_access = true;
    uint64_t permission = eof ? 1 : 2; /* CPU waiter / CPU signaler. */
    Attr attrs[] = {ATTR(SYNC_CPU_ACCESS, cpu_access), ATTR(SYNC_PERMISSION, permission)};
    CHECK(api.NvSciSyncAttrListCreate(module, &engine));
    CHECK(api.NvSciSyncAttrListCreate(module, &cpu));
    CHECK(api.NvMediaIEPFillNvSciSyncAttrList(iep, engine, eof ? IEP_SIGNALER : IEP_WAITER));
    CHECK(api.NvSciSyncAttrListSetAttrs(cpu, attrs, sizeof(attrs) / sizeof(attrs[0])));
    SyncAttrs lists[] = {engine, cpu};
    CHECK(api.NvSciSyncAttrListReconcile(lists, 2, &reconciled, &conflict));
    CHECK(api.NvSciSyncObjAlloc(reconciled, out));
    result = 0;
cleanup:
    if (conflict)
        api.NvSciSyncAttrListFree(conflict);
    if (reconciled)
        api.NvSciSyncAttrListFree(reconciled);
    if (cpu)
        api.NvSciSyncAttrListFree(cpu);
    if (engine)
        api.NvSciSyncAttrListFree(engine);
    return result;
}

static unsigned parse_number(const char *text, unsigned maximum) {
    char *end;
    errno = 0;
    unsigned long value = strtoul(text, &end, 10);
    if (errno || *text == '\0' || *end || value == 0 || value > maximum)
        return 0;
    return (unsigned)value;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if (argc != 8 || (strcmp(argv[7], "lossless") && strcmp(argv[7], "control"))) {
        fprintf(stderr,
                "usage: %s input.y444 encoded.h265 upload.y444 width height "
                "capacity lossless|control\n",
                argv[0]);
        return 2;
    }
    uint32_t width = parse_number(argv[4], 4096), height = parse_number(argv[5], 4096);
    uint32_t capacity = parse_number(argv[6], 256 * 1024 * 1024);
    if (!width || !height || !capacity || width % 32 || height % 2)
        return 2;
    if (load_api() != 0)
        return 1;

    int result = 1;
    BufModule module = NULL;
    BufAttrs request = NULL, reconciled = NULL, conflict = NULL, actual = NULL;
    Buf image = NULL;
    SyncModule sync_module = NULL;
    Sync pre = NULL, eof = NULL;
    WaitContext waiter = NULL;
    Fence pre_fence = {{0}}, eof_fence = {{0}};
    Iep iep = NULL;
    bool image_registered = false, pre_registered = false, eof_registered = false;
    bool submitted = false, completed = false;
    uint8_t *input = NULL, *readback = NULL;
    uint8_t *output = MAP_FAILED;
    size_t mapping_size = 0;
    FILE *file = NULL;
    bool lossless = !strcmp(argv[7], "lossless");
    uint32_t plane_size = width * height;
    size_t input_size = (size_t)plane_size * 3;
    IepVersion version = {0};
    CHECK(api.NvMediaIEPGetVersion(&version));
    printf("iep_version=%u.%u.%u init_size=%zu config_size=%zu picture_size=%zu\n", version.major,
           version.minor, version.patch, sizeof(InitH265), sizeof(ConfigH265), sizeof(PictureH265));
    if (version.major != 1 || version.minor != 1 || version.patch != 0) {
        fprintf(stderr, "unsupported IEP version for reconstructed ABI\n");
        goto cleanup;
    }
    bool buf_compatible = false, sync_compatible = false;
    CHECK(api.NvSciBufCheckVersionCompatibility(2, 11, &buf_compatible));
    CHECK(api.NvSciSyncCheckVersionCompatibility(2, 8, &sync_compatible));
    printf("nvscibuf_2_11=%d nvscisync_2_8=%d\n", buf_compatible, sync_compatible);
    if (!buf_compatible || !sync_compatible)
        goto cleanup;

    long page = sysconf(_SC_PAGESIZE);
    if (page <= 0 || capacity % (unsigned)page)
        goto cleanup;
    mapping_size = capacity + 2 * (size_t)page;
    output = mmap(NULL, mapping_size, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (output == MAP_FAILED || mprotect(output + page, capacity, PROT_READ | PROT_WRITE))
        goto cleanup;
    printf("output_allocated_bytes=%u guard_pages=2\n", capacity);

    input = malloc(input_size);
    readback = malloc(input_size);
    if (!input || !readback)
        goto cleanup;
    file = fopen(argv[1], "rb");
    if (!file || fread(input, 1, input_size, file) != input_size || fgetc(file) != EOF) {
        fprintf(stderr, "expected exactly %zu input bytes\n", input_size);
        goto cleanup;
    }
    fclose(file);
    file = NULL;

    CHECK(api.NvSciBufModuleOpen(&module));
    CHECK(api.NvSciBufAttrListCreate(module, &request));
    CHECK(api.NvMediaIEPFillNvSciBufAttrList(0, request));
    uint32_t image_type = 2, permission = 3, block_linear = 0, progressive = 0;
    uint64_t image_count = 1;
    uint32_t yuv = 0, semiplanar = 1, sample444 = 2, bpc8 = 2, yuv_order = 0, rec709 = 4;
    bool cpu_access = true, cache = false;
    Attr attrs[] = {ATTR(BUF_TYPES, image_type),      ATTR(BUF_CPU_ACCESS, cpu_access),
                    ATTR(BUF_PERMISSION, permission), ATTR(BUF_CPU_CACHE, cache),
                    ATTR(IMAGE_LAYOUT, block_linear), ATTR(IMAGE_SCAN, progressive),
                    ATTR(IMAGE_COUNT, image_count),   ATTR(SURFACE_TYPE, yuv),
                    ATTR(SURFACE_MEMORY, semiplanar), ATTR(SURFACE_SAMPLE, sample444),
                    ATTR(SURFACE_BPC, bpc8),          ATTR(SURFACE_ORDER, yuv_order),
                    ATTR(SURFACE_WIDTH, width),       ATTR(SURFACE_HEIGHT, height),
                    ATTR(SURFACE_COLOR, rec709)};
    for (size_t index = 0; index < sizeof(attrs) / sizeof(attrs[0]); ++index) {
        printf("attribute_key=0x%x bytes=%zu\n", attrs[index].key, attrs[index].len);
        CHECK(api.NvSciBufAttrListSetAttrs(request, &attrs[index], 1));
    }
    CHECK(api.NvSciBufAttrListReconcile(&request, 1, &reconciled, &conflict));
    CHECK(api.NvSciBufObjAlloc(reconciled, &image));
    CHECK(api.NvSciBufObjGetAttrList(image, &actual));
    Attr query[] = {{IMAGE_SIZE, NULL, 0},   {BUF_CACHE_COHERENCY, NULL, 0},
                    {IMAGE_PLANES, NULL, 0}, {IMAGE_FORMAT, NULL, 0},
                    {IMAGE_WIDTH, NULL, 0},  {IMAGE_HEIGHT, NULL, 0}};
    CHECK(api.NvSciBufAttrListGetAttrs(actual, query, sizeof(query) / sizeof(query[0])));
    uint64_t image_bytes;
    bool flush;
    uint32_t planes;
    if (query[0].len != sizeof(image_bytes) || query[1].len != sizeof(flush) ||
        query[2].len != sizeof(planes))
        goto cleanup;
    memcpy(&image_bytes, query[0].value, sizeof(image_bytes));
    memcpy(&flush, query[1].value, sizeof(flush));
    memcpy(&planes, query[2].value, sizeof(planes));
    printf("surface_bytes=%llu planes=%u cache_flush=%d\n", (unsigned long long)image_bytes, planes,
           flush);
    if (planes != 2 || query[3].len != 8 || query[4].len != 8 || query[5].len != 8)
        goto cleanup;
    uint32_t formats[2], widths[2], heights[2];
    memcpy(formats, query[3].value, sizeof(formats));
    memcpy(widths, query[4].value, sizeof(widths));
    memcpy(heights, query[5].value, sizeof(heights));
    printf("surface_formats=%u,%u widths=%u,%u heights=%u,%u\n", formats[0], formats[1], widths[0],
           widths[1], heights[0], heights[1]);
    if (formats[0] != COLOR_Y8 || formats[1] != COLOR_V8U8 || widths[0] != width ||
        widths[1] != width || heights[0] != height || heights[1] != height)
        goto cleanup;
    const void *src[] = {input, input + plane_size, input + 2 * plane_size};
    void *dst[] = {readback, readback + plane_size, readback + 2 * plane_size};
    uint32_t sizes[] = {plane_size, plane_size, plane_size}, pitches[] = {width, width, width};
    CHECK(api.NvSciBufObjPutPixels(image, NULL, src, sizes, pitches));
    if (flush)
        CHECK(api.NvSciBufObjFlushCpuCacheRange(image, 0, image_bytes));
    CHECK(api.NvSciBufObjGetPixels(image, NULL, dst, sizes, pitches));
    if (memcmp(input, readback, input_size)) {
        fprintf(stderr, "NvSciBuf upload roundtrip differs\n");
        goto cleanup;
    }
    file = fopen(argv[3], "wbx");
    if (!file || fwrite(readback, 1, input_size, file) != input_size)
        goto cleanup;
    if (fclose(file) != 0) {
        file = NULL;
        goto cleanup;
    }
    file = NULL;
    printf("upload_exact=true\n");

    InitH265 init = {0};
    init.width = (uint16_t)width;
    init.height = (uint16_t)height;
    init.fps_num = 15;
    init.fps_den = 1;
    init.profile = PROFILE_REXT;
    init.max_refs = 1;
    init.disable_memory_optimization = 1;
    iep = api.NvMediaIEPCreate(CODEC_HEVC, &init, actual, 4, 0);
    printf("encoder_created=%s\n", iep ? "true" : "false");
    if (!iep)
        goto cleanup;
    ConfigH265 config = {0};
    config.features = FEATURE_AUD | (lossless ? FEATURE_LOSSLESS : 0);
    config.gop = 1;
    config.idr_period = 1;
    config.repeat_headers = 2;
    config.rc.mode = RC_CONSTANT_QP;
    config.rc.params.constant =
        (Qp){.p = lossless ? 0 : 20, .b = lossless ? 0 : 20, .i = lossless ? 0 : 20};
    config.rc.max_session_qp = 51;
    config.preset = 0x7fffffff;
    CHECK(api.NvMediaIEPSetConfiguration(iep, &config));
    CHECK(api.NvMediaIEPRegisterNvSciBufObj(iep, image));
    image_registered = true;
    CHECK(api.NvSciSyncModuleOpen(&sync_module));
    CHECK(api.NvSciSyncCpuWaitContextAlloc(sync_module, &waiter));
    if (make_sync(sync_module, iep, false, &pre) || make_sync(sync_module, iep, true, &eof))
        goto cleanup;
    CHECK(api.NvMediaIEPRegisterNvSciSyncObj(iep, IEP_PRE, pre));
    pre_registered = true;
    CHECK(api.NvMediaIEPRegisterNvSciSyncObj(iep, IEP_EOF, eof));
    eof_registered = true;
    CHECK(api.NvMediaIEPSetNvSciSyncObjforEOF(iep, eof));
    CHECK(api.NvSciSyncObjGenerateFence(pre, &pre_fence));
    CHECK(api.NvMediaIEPInsertPreNvSciSyncFence(iep, &pre_fence));
    CHECK(api.NvSciSyncObjSignal(pre));
    PictureH265 picture = {0};
    picture.type = PIC_IDR;
    picture.flags = PIC_HEADERS;
    picture.rc = config.rc;
    picture.fps_num = 15;
    picture.fps_den = 1;
    CHECK(api.NvMediaIEPFeedFrame(iep, image, &picture, 0));
    submitted = true;
    CHECK(api.NvMediaIEPGetEOFNvSciSyncFence(iep, eof, &eof_fence));
    CHECK(api.NvSciSyncFenceWait(&eof_fence, waiter, 10000000));
    completed = true;
    uint32_t available = 0, used = 0;
    CHECK(api.NvMediaIEPBitsAvailable(iep, &available, 1, 10000));
    printf("available_bytes=%u capacity_bytes=%u\n", available, capacity);
    if (!available || available > capacity)
        goto cleanup;
    Bitstream stream = {.data = output + page, .capacity = capacity};
    CHECK(api.NvMediaIEPGetBits(iep, &used, 1, &stream, NULL));
    printf("encoded_bytes=%u bitstream_bytes=%u\n", used, stream.used);
    /* This runtime leaves bitstreamBytes at zero; numBytes is authoritative. */
    if (used != available || used > capacity || (stream.used && used != stream.used))
        goto cleanup;
    file = fopen(argv[2], "wbx");
    if (!file || fwrite(stream.data, 1, used, file) != used)
        goto cleanup;
    if (fclose(file) != 0) {
        file = NULL;
        goto cleanup;
    }
    file = NULL;
    if (mapped_libraries())
        goto cleanup;
    result = 0;
cleanup:
    if (file)
        fclose(file);
    if (result)
        mapped_libraries();
    /* Failed asynchronous work is left to process teardown; never free a live
     * surface. */
    if (!submitted || completed) {
        if (iep && image_registered)
            api.NvMediaIEPUnregisterNvSciBufObj(iep, image);
        if (iep && pre_registered)
            api.NvMediaIEPUnregisterNvSciSyncObj(iep, pre);
        if (iep && eof_registered)
            api.NvMediaIEPUnregisterNvSciSyncObj(iep, eof);
        api.NvSciSyncFenceClear(&pre_fence);
        api.NvSciSyncFenceClear(&eof_fence);
        if (iep)
            api.NvMediaIEPDestroy(iep);
        if (pre)
            api.NvSciSyncObjFree(pre);
        if (eof)
            api.NvSciSyncObjFree(eof);
        if (waiter)
            api.NvSciSyncCpuWaitContextFree(waiter);
        if (sync_module)
            api.NvSciSyncModuleClose(sync_module);
        if (image)
            api.NvSciBufObjFree(image);
        if (conflict)
            api.NvSciBufAttrListFree(conflict);
        if (reconciled)
            api.NvSciBufAttrListFree(reconciled);
        if (request)
            api.NvSciBufAttrListFree(request);
        if (module)
            api.NvSciBufModuleClose(module);
    }
    if (output != MAP_FAILED)
        munmap(output, mapping_size);
    free(input);
    free(readback);
    printf("encode_passed=%s\n", result ? "false" : "true");
    return result;
}
