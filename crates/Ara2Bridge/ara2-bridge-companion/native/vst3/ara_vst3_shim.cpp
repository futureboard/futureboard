#include "ara_vst3_shim.hpp"

#include "ARAVST3.h"

#include <atomic>
#include <cstring>
#include <stdexcept>

namespace {

using Steinberg::FUnknown;
using Steinberg::FUnknownPrivate::iidEqual;
using Steinberg::kNoInterface;
using Steinberg::kResultOk;
using Steinberg::tresult;
using Steinberg::uint32;

constexpr uint32 kUnknownWords[4] = {0x00000000, 0x00000000, 0xC0000000, 0x00000046};
constexpr uint32 kMainFactoryWords[4] = {0xDB2A1669, 0xFAFD42A5, 0xA82F864F, 0x7B6872EA};
constexpr uint32 kEntryWords[4] = {0x12814E54, 0xA1CE4076, 0x82B96813, 0x16950BD6};
constexpr uint32 kEntry2Words[4] = {0xCD9A5913, 0xC9EB46D7, 0x96CA53AD, 0xD1DB89F5};

bool isUnknownIid(const Steinberg::TUID iid) noexcept
{
    return iidEqual(iid, Steinberg::FUnknown_iid);
}

class MainFactoryAdapter final : public ARA::IMainFactory
{
public:
    explicit MainFactoryAdapter(const Ara2Vst3MainFactoryCallbacks& callbacks) noexcept
    : _callbacks(callbacks)
    {
    }

    ~MainFactoryAdapter()
    {
        if (_callbacks.drop)
            _callbacks.drop(_callbacks.context);
    }

    tresult PLUGIN_API queryInterface(const Steinberg::TUID iid, void** object) override
    {
        if (!object)
            return kNoInterface;
        *object = nullptr;
        if (iidEqual(iid, ARA::IMainFactory_iid) || isUnknownIid(iid))
        {
            *object = static_cast<ARA::IMainFactory*>(this);
            addRef();
            return kResultOk;
        }
        return kNoInterface;
    }

    uint32 PLUGIN_API addRef() override { return ++_references; }

    uint32 PLUGIN_API release() override
    {
        const auto references { --_references };
        if (references == 0)
            delete this;
        return references;
    }

    const ARA::ARAFactory* PLUGIN_API getFactory() override
    {
        return static_cast<const ARA::ARAFactory*>(_callbacks.get_factory(_callbacks.context));
    }

private:
    std::atomic<uint32> _references { 1 };
    Ara2Vst3MainFactoryCallbacks _callbacks;
};

class PluginEntryAdapter final : public ARA::IPlugInEntryPoint, public ARA::IPlugInEntryPoint2
{
public:
    explicit PluginEntryAdapter(const Ara2Vst3PluginEntryCallbacks& callbacks) noexcept
    : _callbacks(callbacks)
    {
    }

    ~PluginEntryAdapter()
    {
        if (_callbacks.drop)
            _callbacks.drop(_callbacks.context);
    }

    tresult PLUGIN_API queryInterface(const Steinberg::TUID iid, void** object) override
    {
        if (!object)
            return kNoInterface;
        *object = nullptr;
        if (iidEqual(iid, ARA::IPlugInEntryPoint_iid) || isUnknownIid(iid))
            *object = static_cast<ARA::IPlugInEntryPoint*>(this);
        else if (iidEqual(iid, ARA::IPlugInEntryPoint2_iid))
            *object = static_cast<ARA::IPlugInEntryPoint2*>(this);
        else
            return kNoInterface;
        addRef();
        return kResultOk;
    }

    uint32 PLUGIN_API addRef() override { return ++_references; }

    uint32 PLUGIN_API release() override
    {
        const auto references { --_references };
        if (references == 0)
            delete this;
        return references;
    }

    const ARA::ARAFactory* PLUGIN_API getFactory() override
    {
        return static_cast<const ARA::ARAFactory*>(_callbacks.get_factory(_callbacks.context));
    }

    const ARA::ARAPlugInExtensionInstance* PLUGIN_API bindToDocumentController(
        ARA::ARADocumentControllerRef documentControllerRef) override
    {
        return static_cast<const ARA::ARAPlugInExtensionInstance*>(
            _callbacks.bind(_callbacks.context, documentControllerRef, 0, 0));
    }

    const ARA::ARAPlugInExtensionInstance* PLUGIN_API bindToDocumentControllerWithRoles(
        ARA::ARADocumentControllerRef documentControllerRef,
        ARA::ARAPlugInInstanceRoleFlags knownRoles,
        ARA::ARAPlugInInstanceRoleFlags assignedRoles) override
    {
        return static_cast<const ARA::ARAPlugInExtensionInstance*>(
            _callbacks.bind(_callbacks.context, documentControllerRef, knownRoles, assignedRoles));
    }

private:
    std::atomic<uint32> _references { 1 };
    Ara2Vst3PluginEntryCallbacks _callbacks;
};

const Steinberg::int8* interfaceIid(Ara2Vst3InterfaceKind kind) noexcept
{
    switch (kind)
    {
        case ARA2_VST3_INTERFACE_UNKNOWN: return Steinberg::FUnknown_iid;
        case ARA2_VST3_INTERFACE_MAIN_FACTORY: return ARA::IMainFactory_iid;
        case ARA2_VST3_INTERFACE_PLUGIN_ENTRY: return ARA::IPlugInEntryPoint_iid;
        case ARA2_VST3_INTERFACE_PLUGIN_ENTRY_2: return ARA::IPlugInEntryPoint2_iid;
        default: return nullptr;
    }
}

Ara2Vst3Result mapQueryResult(tresult result) noexcept
{
    return (result == kResultOk) ? ARA2_VST3_OK : ARA2_VST3_NO_INTERFACE;
}

template <typename Interface, typename Callback>
Ara2Vst3Result withInterface(
    void* unknown,
    const Steinberg::TUID iid,
    Callback&& callback)
{
    if (!unknown)
        return ARA2_VST3_INVALID_ARGUMENT;
    Interface* interfaceObject { nullptr };
    const auto result {
        static_cast<FUnknown*>(unknown)->queryInterface(iid, reinterpret_cast<void**>(&interfaceObject))
    };
    if (result != kResultOk || !interfaceObject)
        return ARA2_VST3_NO_INTERFACE;
    const auto callbackResult { callback(interfaceObject) };
    interfaceObject->release();
    return callbackResult;
}

} // namespace

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_interface_id(
    Ara2Vst3InterfaceKind kind,
    Ara2Vst3InterfaceId* output)
{
    try
    {
        if (!output)
            return ARA2_VST3_INVALID_ARGUMENT;
        const uint32* words { nullptr };
        switch (kind)
        {
            case ARA2_VST3_INTERFACE_UNKNOWN: words = kUnknownWords; break;
            case ARA2_VST3_INTERFACE_MAIN_FACTORY: words = kMainFactoryWords; break;
            case ARA2_VST3_INTERFACE_PLUGIN_ENTRY: words = kEntryWords; break;
            case ARA2_VST3_INTERFACE_PLUGIN_ENTRY_2: words = kEntry2Words; break;
            default: return ARA2_VST3_INVALID_ARGUMENT;
        }
        std::memcpy(output->words, words, sizeof(output->words));
        return ARA2_VST3_OK;
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" const char* ARA2_VST3_CALL ara2_vst3_main_factory_category(void)
{
    try { return kARAMainFactoryClass; }
    catch (...) { return nullptr; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_main_factory_create(
    const Ara2Vst3MainFactoryCallbacks* callbacks,
    void** output)
{
    try
    {
        if (!callbacks || !callbacks->get_factory || !output)
            return ARA2_VST3_INVALID_ARGUMENT;
        *output = static_cast<ARA::IMainFactory*>(new MainFactoryAdapter(*callbacks));
        return ARA2_VST3_OK;
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_plugin_entry_create(
    const Ara2Vst3PluginEntryCallbacks* callbacks,
    void** output)
{
    try
    {
        if (!callbacks || !callbacks->get_factory || !callbacks->bind || !output)
            return ARA2_VST3_INVALID_ARGUMENT;
        *output = static_cast<ARA::IPlugInEntryPoint*>(new PluginEntryAdapter(*callbacks));
        return ARA2_VST3_OK;
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_query_interface(
    void* unknown,
    Ara2Vst3InterfaceKind kind,
    void** output)
{
    try
    {
        if (!unknown || !output)
            return ARA2_VST3_INVALID_ARGUMENT;
        *output = nullptr;
        const auto* iid { interfaceIid(kind) };
        if (!iid)
            return ARA2_VST3_INVALID_ARGUMENT;
        return mapQueryResult(static_cast<FUnknown*>(unknown)->queryInterface(iid, output));
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_add_ref(void* unknown, uint32_t* output)
{
    try
    {
        if (!unknown || !output)
            return ARA2_VST3_INVALID_ARGUMENT;
        *output = static_cast<FUnknown*>(unknown)->addRef();
        return ARA2_VST3_OK;
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_release(void* unknown, uint32_t* output)
{
    try
    {
        if (!unknown || !output)
            return ARA2_VST3_INVALID_ARGUMENT;
        *output = static_cast<FUnknown*>(unknown)->release();
        return ARA2_VST3_OK;
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_main_factory_get_factory(
    void* unknown,
    const void** output)
{
    try
    {
        if (!output)
            return ARA2_VST3_INVALID_ARGUMENT;
        return withInterface<ARA::IMainFactory>(unknown, ARA::IMainFactory_iid,
            [output](ARA::IMainFactory* factory) {
                const auto* araFactory { factory->getFactory() };
                if (!araFactory)
                    return static_cast<Ara2Vst3Result>(ARA2_VST3_PEER_ERROR);
                *output = araFactory;
                return static_cast<Ara2Vst3Result>(ARA2_VST3_OK);
            });
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_plugin_entry_get_factory(
    void* unknown,
    const void** output)
{
    try
    {
        if (!output)
            return ARA2_VST3_INVALID_ARGUMENT;
        return withInterface<ARA::IPlugInEntryPoint>(unknown, ARA::IPlugInEntryPoint_iid,
            [output](ARA::IPlugInEntryPoint* entry) {
                const auto* araFactory { entry->getFactory() };
                if (!araFactory)
                    return static_cast<Ara2Vst3Result>(ARA2_VST3_PEER_ERROR);
                *output = araFactory;
                return static_cast<Ara2Vst3Result>(ARA2_VST3_OK);
            });
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_plugin_entry_bind(
    void* unknown,
    void* document_controller,
    int32_t known_roles,
    int32_t assigned_roles,
    uint8_t use_role_aware_entry,
    const void** output)
{
    try
    {
        if (!document_controller || !output)
            return ARA2_VST3_INVALID_ARGUMENT;
        if (use_role_aware_entry)
        {
            return withInterface<ARA::IPlugInEntryPoint2>(unknown, ARA::IPlugInEntryPoint2_iid,
                [=](ARA::IPlugInEntryPoint2* entry) {
                    const auto* extension { entry->bindToDocumentControllerWithRoles(
                        static_cast<ARA::ARADocumentControllerRef>(document_controller),
                        known_roles, assigned_roles) };
                    if (!extension)
                        return static_cast<Ara2Vst3Result>(ARA2_VST3_PEER_ERROR);
                    *output = extension;
                    return static_cast<Ara2Vst3Result>(ARA2_VST3_OK);
                });
        }
        return withInterface<ARA::IPlugInEntryPoint>(unknown, ARA::IPlugInEntryPoint_iid,
            [=](ARA::IPlugInEntryPoint* entry) {
                const auto* extension { entry->bindToDocumentController(
                    static_cast<ARA::ARADocumentControllerRef>(document_controller)) };
                if (!extension)
                    return static_cast<Ara2Vst3Result>(ARA2_VST3_PEER_ERROR);
                *output = extension;
                return static_cast<Ara2Vst3Result>(ARA2_VST3_OK);
            });
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}

extern "C" Ara2Vst3Result ARA2_VST3_CALL ara2_vst3_probe_exception_boundary(
    uint8_t throw_exception)
{
    try
    {
        if (throw_exception)
            throw std::runtime_error("ARA VST3 shim exception probe");
        return ARA2_VST3_OK;
    }
    catch (...) { return ARA2_VST3_EXCEPTION; }
}
