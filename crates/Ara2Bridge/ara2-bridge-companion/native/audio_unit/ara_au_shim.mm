#include "ara_au_shim.h"

#include "AUBase.h"
#include "ARAAudioUnit.h"

#include <cstddef>
#include <new>

namespace {

struct PluginHandler
{
    explicit PluginHandler(const Ara2AudioUnitPluginCallbacks& callbacks) noexcept
    : callbacks(callbacks)
    {
    }

    ~PluginHandler()
    {
        if (callbacks.drop)
            callbacks.drop(callbacks.context);
    }

    Ara2AudioUnitPluginCallbacks callbacks;
};

constexpr uint32_t kLegacyBindingSize {
    static_cast<uint32_t>(offsetof(ARA::ARAAudioUnitPlugInExtensionBinding, knownRoles))
};

bool isAraProperty(uint32_t property) noexcept
{
    return property == ARA::kAudioUnitProperty_ARAFactory ||
           property == ARA::kAudioUnitProperty_ARAPlugInExtensionBinding ||
           property == ARA::kAudioUnitProperty_ARAPlugInExtensionBindingWithRoles;
}

OSStatus validateAddress(uint32_t property, uint32_t scope, uint32_t element) noexcept
{
    if (!isAraProperty(property))
        return kAudioUnitErr_InvalidProperty;
    if (scope != kAudioUnitScope_Global)
        return kAudioUnitErr_InvalidScope;
    if (element != 0)
        return kAudioUnitErr_InvalidElement;
    return noErr;
}

OSStatus exceptionStatus() noexcept
{
    return kAudioUnitErr_CannotDoInCurrentContext;
}

} // namespace

extern "C" int32_t ara2_audio_unit_plugin_create(
    const Ara2AudioUnitPluginCallbacks* callbacks,
    void** output)
{
    try
    {
        if (!callbacks || !callbacks->get_factory || !callbacks->bind || !output)
            return paramErr;
        *output = new PluginHandler(*callbacks);
        return noErr;
    }
    catch (...) { return exceptionStatus(); }
}

extern "C" void ara2_audio_unit_plugin_destroy(void* handler)
{
    try { delete static_cast<PluginHandler*>(handler); }
    catch (...) { }
}

extern "C" int32_t ara2_audio_unit_plugin_get_property_info(
    void* handler,
    uint32_t property,
    uint32_t scope,
    uint32_t element,
    uint32_t* output_size,
    uint8_t* output_writable)
{
    try
    {
        if (!handler || !output_size || !output_writable)
            return paramErr;
        const auto addressStatus { validateAddress(property, scope, element) };
        if (addressStatus != noErr)
            return addressStatus;
        *output_size = (property == ARA::kAudioUnitProperty_ARAFactory)
            ? static_cast<uint32_t>(sizeof(ARA::ARAAudioUnitFactory))
            : static_cast<uint32_t>(sizeof(ARA::ARAAudioUnitPlugInExtensionBinding));
        *output_writable = 0;
        return noErr;
    }
    catch (...) { return exceptionStatus(); }
}

extern "C" int32_t ara2_audio_unit_plugin_get_property(
    void* handler,
    uint32_t property,
    uint32_t scope,
    uint32_t element,
    void* data,
    uint32_t data_size)
{
    try
    {
        if (!handler || !data)
            return paramErr;
        const auto addressStatus { validateAddress(property, scope, element) };
        if (addressStatus != noErr)
            return addressStatus;
        auto* const state { static_cast<PluginHandler*>(handler) };
        if (property == ARA::kAudioUnitProperty_ARAFactory)
        {
            if (data_size != sizeof(ARA::ARAAudioUnitFactory))
                return kAudioUnitErr_InvalidPropertyValue;
            auto* const record { static_cast<ARA::ARAAudioUnitFactory*>(data) };
            if (record->inOutMagicNumber != ARA::kARAAudioUnitMagic)
                return kAudioUnitErr_InvalidProperty;
            const auto* factory { state->callbacks.get_factory(state->callbacks.context) };
            if (!factory)
                return kAudioUnitErr_CannotDoInCurrentContext;
            record->outFactory = static_cast<const ARA::ARAFactory*>(factory);
            return noErr;
        }

        const bool legacy { property == ARA::kAudioUnitProperty_ARAPlugInExtensionBinding };
        const auto expectedSize { legacy ? kLegacyBindingSize :
            static_cast<uint32_t>(sizeof(ARA::ARAAudioUnitPlugInExtensionBinding)) };
        if (data_size != expectedSize)
            return kAudioUnitErr_InvalidPropertyValue;
        auto* const record { static_cast<ARA::ARAAudioUnitPlugInExtensionBinding*>(data) };
        if (record->inOutMagicNumber != ARA::kARAAudioUnitMagic ||
            !record->inDocumentControllerRef)
            return kAudioUnitErr_InvalidProperty;

        constexpr auto allRoles { ARA::kARAPlaybackRendererRole |
                                  ARA::kARAEditorRendererRole |
                                  ARA::kARAEditorViewRole };
        const auto knownRoles { legacy ? allRoles : record->knownRoles };
        const auto assignedRoles { legacy ? allRoles : record->assignedRoles };
        const auto* extension { state->callbacks.bind(
            state->callbacks.context,
            record->inDocumentControllerRef,
            knownRoles,
            assignedRoles) };
        if (!extension)
            return kAudioUnitErr_CannotDoInCurrentContext;
        record->outPlugInExtension =
            static_cast<const ARA::ARAPlugInExtensionInstance*>(extension);
        return noErr;
    }
    catch (...) { return exceptionStatus(); }
}

extern "C" int32_t ara2_audio_unit_host_get_factory(void* audio_unit, const void** output)
{
    try
    {
        if (!audio_unit || !output)
            return paramErr;
        UInt32 propertySize { sizeof(ARA::ARAAudioUnitFactory) };
        Boolean writable { true };
        const auto unit { static_cast<AudioUnit>(audio_unit) };
        auto status { AudioUnitGetPropertyInfo(unit,
            ARA::kAudioUnitProperty_ARAFactory, kAudioUnitScope_Global, 0,
            &propertySize, &writable) };
        if (status != noErr || propertySize != sizeof(ARA::ARAAudioUnitFactory) || writable)
            return (status != noErr) ? status : kAudioUnitErr_InvalidPropertyValue;
        ARA::ARAAudioUnitFactory record { ARA::kARAAudioUnitMagic, nullptr };
        status = AudioUnitGetProperty(unit, ARA::kAudioUnitProperty_ARAFactory,
            kAudioUnitScope_Global, 0, &record, &propertySize);
        if (status != noErr)
            return status;
        if (propertySize != sizeof(record) || record.inOutMagicNumber != ARA::kARAAudioUnitMagic ||
            !record.outFactory)
            return kAudioUnitErr_InvalidPropertyValue;
        *output = record.outFactory;
        return noErr;
    }
    catch (...) { return exceptionStatus(); }
}

extern "C" int32_t ara2_audio_unit_host_bind(
    void* audio_unit,
    void* document_controller,
    int32_t known_roles,
    int32_t assigned_roles,
    uint8_t allow_legacy_fallback,
    const void** output)
{
    try
    {
        if (!audio_unit || !document_controller || !output)
            return paramErr;
        ARA::ARAAudioUnitPlugInExtensionBinding record {
            ARA::kARAAudioUnitMagic,
            static_cast<ARA::ARADocumentControllerRef>(document_controller),
            nullptr,
            known_roles,
            assigned_roles
        };
        UInt32 propertySize { sizeof(record) };
        const auto unit { static_cast<AudioUnit>(audio_unit) };
        auto status { AudioUnitGetProperty(unit,
            ARA::kAudioUnitProperty_ARAPlugInExtensionBindingWithRoles,
            kAudioUnitScope_Global, 0, &record, &propertySize) };
        UInt32 expectedSize { sizeof(record) };
        if (status != noErr && allow_legacy_fallback)
        {
            propertySize = kLegacyBindingSize;
            expectedSize = kLegacyBindingSize;
            status = AudioUnitGetProperty(unit,
                ARA::kAudioUnitProperty_ARAPlugInExtensionBinding,
                kAudioUnitScope_Global, 0, &record, &propertySize);
        }
        if (status != noErr)
            return status;
        if (propertySize != expectedSize || record.inOutMagicNumber != ARA::kARAAudioUnitMagic ||
            record.inDocumentControllerRef != document_controller || !record.outPlugInExtension)
            return kAudioUnitErr_InvalidPropertyValue;
        *output = record.outPlugInExtension;
        return noErr;
    }
    catch (...) { return exceptionStatus(); }
}
