#ifndef ARA2_BRIDGE_VST3_SHIM_HPP
#define ARA2_BRIDGE_VST3_SHIM_HPP

#include <stdint.h>

#if defined(_WIN32)
#define ARA2_VST3_CALL __cdecl
#else
#define ARA2_VST3_CALL
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef int32_t Ara2Vst3Result;

enum {
    ARA2_VST3_OK = 0,
    ARA2_VST3_INVALID_ARGUMENT = -1,
    ARA2_VST3_NO_INTERFACE = -2,
    ARA2_VST3_PEER_ERROR = -3,
    ARA2_VST3_EXCEPTION = -4
};

typedef enum Ara2Vst3InterfaceKind {
    ARA2_VST3_INTERFACE_UNKNOWN = 0,
    ARA2_VST3_INTERFACE_MAIN_FACTORY = 1,
    ARA2_VST3_INTERFACE_PLUGIN_ENTRY = 2,
    ARA2_VST3_INTERFACE_PLUGIN_ENTRY_2 = 3
} Ara2Vst3InterfaceKind;

typedef struct Ara2Vst3InterfaceId {
    uint32_t words[4];
} Ara2Vst3InterfaceId;

typedef const void* (ARA2_VST3_CALL *Ara2Vst3GetFactoryCallback)(void* context);
typedef const void* (ARA2_VST3_CALL *Ara2Vst3BindCallback)(
    void* context,
    void* document_controller,
    int32_t known_roles,
    int32_t assigned_roles);
typedef void (ARA2_VST3_CALL *Ara2Vst3DropCallback)(void* context);

typedef struct Ara2Vst3MainFactoryCallbacks {
    void* context;
    Ara2Vst3GetFactoryCallback get_factory;
    Ara2Vst3DropCallback drop;
} Ara2Vst3MainFactoryCallbacks;

typedef struct Ara2Vst3PluginEntryCallbacks {
    void* context;
    Ara2Vst3GetFactoryCallback get_factory;
    Ara2Vst3BindCallback bind;
    Ara2Vst3DropCallback drop;
} Ara2Vst3PluginEntryCallbacks;

Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_interface_id(
    Ara2Vst3InterfaceKind kind,
    Ara2Vst3InterfaceId* output);
const char* ARA2_VST3_CALL ara2_vst3_main_factory_category(void);

Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_main_factory_create(
    const Ara2Vst3MainFactoryCallbacks* callbacks,
    void** output);
Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_plugin_entry_create(
    const Ara2Vst3PluginEntryCallbacks* callbacks,
    void** output);

Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_query_interface(
    void* unknown,
    Ara2Vst3InterfaceKind kind,
    void** output);
Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_add_ref(void* unknown, uint32_t* output);
Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_release(void* unknown, uint32_t* output);

Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_main_factory_get_factory(
    void* unknown,
    const void** output);
Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_plugin_entry_get_factory(
    void* unknown,
    const void** output);
Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_plugin_entry_bind(
    void* unknown,
    void* document_controller,
    int32_t known_roles,
    int32_t assigned_roles,
    uint8_t use_role_aware_entry,
    const void** output);

Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_probe_exception_boundary(uint8_t throw_exception);

#ifdef __cplusplus
}
#endif

#endif
