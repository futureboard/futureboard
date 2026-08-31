#ifndef ARA2_BRIDGE_AUDIO_UNIT_SHIM_H
#define ARA2_BRIDGE_AUDIO_UNIT_SHIM_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef const void* (*Ara2AudioUnitGetFactoryCallback)(void* context);
typedef const void* (*Ara2AudioUnitBindCallback)(
    void* context,
    void* document_controller,
    int32_t known_roles,
    int32_t assigned_roles);
typedef void (*Ara2AudioUnitDropCallback)(void* context);

typedef struct Ara2AudioUnitPluginCallbacks {
    void* context;
    Ara2AudioUnitGetFactoryCallback get_factory;
    Ara2AudioUnitBindCallback bind;
    Ara2AudioUnitDropCallback drop;
} Ara2AudioUnitPluginCallbacks;

int32_t ara2_audio_unit_plugin_create(
    const Ara2AudioUnitPluginCallbacks* callbacks,
    void** output);
void ara2_audio_unit_plugin_destroy(void* handler);
int32_t ara2_audio_unit_plugin_get_property_info(
    void* handler,
    uint32_t property,
    uint32_t scope,
    uint32_t element,
    uint32_t* output_size,
    uint8_t* output_writable);
int32_t ara2_audio_unit_plugin_get_property(
    void* handler,
    uint32_t property,
    uint32_t scope,
    uint32_t element,
    void* data,
    uint32_t data_size);
int32_t ara2_audio_unit_host_get_factory(void* audio_unit, const void** output);
int32_t ara2_audio_unit_host_bind(
    void* audio_unit,
    void* document_controller,
    int32_t known_roles,
    int32_t assigned_roles,
    uint8_t allow_legacy_fallback,
    const void** output);

#ifdef __cplusplus
}
#endif

#endif
