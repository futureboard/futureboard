#include "sphere_plugin_host_vst3.h"

#include "public.sdk/source/vst/hosting/hostclasses.h"
#include "public.sdk/source/vst/hosting/module.h"
#include "public.sdk/source/vst/utility/stringconvert.h"

#include "clap/clap.h"
#include "clap/factory/plugin-factory.h"

#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <filesystem>
#include <new>
#include <string>
#include <vector>

#ifdef _WIN32
#  define WIN32_LEAN_AND_MEAN
#  include <windows.h>
#else
#  include <dlfcn.h>
#endif

namespace {

// Match VST3 SDK string literals exactly.
constexpr const char* kAudioModuleClass = "Audio Module Class";

SpherePluginHostString make_string(std::string value) {
  auto* data = new (std::nothrow) char[value.size() + 1];
  if (!data) {
    return {nullptr, 0};
  }
  std::memcpy(data, value.data(), value.size());
  data[value.size()] = '\0';
  return {data, static_cast<unsigned long long>(value.size())};
}

std::string escape_json(const std::string& value) {
  std::string out;
  out.reserve(value.size() + 8);
  for (char c : value) {
    if (c == '\\' || c == '"') {
      out.push_back('\\');
    }
    if (c == '\n') {
      out += "\\n";
      continue;
    }
    if (c == '\r') {
      out += "\\r";
      continue;
    }
    if (c == '\t') {
      out += "\\t";
      continue;
    }
    out.push_back(c);
  }
  return out;
}

std::string lower_extension(const std::filesystem::path& path) {
  auto ext = path.extension().string();
  for (auto& c : ext) {
    c = static_cast<char>(std::tolower(static_cast<unsigned char>(c)));
  }
  return ext;
}

bool is_vst3_bundle(const std::filesystem::path& path) {
  return lower_extension(path) == ".vst3";
}

bool is_clap_plugin(const std::filesystem::path& path) {
  return lower_extension(path) == ".clap";
}

std::string uid_to_string(const Steinberg::TUID cid) {
  return VST3::UID::fromTUID(cid).toString();
}

template <typename T>
std::string char_array_to_string(const T* value, Steinberg::uint32 max) {
  return Steinberg::Vst::StringConvert::convert(value, max);
}

struct ClassEntry {
  std::string name;
  std::string vendor;
  std::string category;
  std::string sub_categories;
  std::string class_id;
  std::string version;
  std::string sdk_version;
};

/// Emit an entry for a module whose metadata could not be read.
///
/// `sdkMetadataLoaded:false` plus `loadError` is what marks it: the Rust side
/// turns these into scan *failures* rather than plug-in rows, so a broken or
/// wrong-architecture module stays reportable by path and reason instead of
/// appearing in the browser as a plug-in named after its file.
void append_fallback_entry(std::string& json, bool& first,
                           const std::filesystem::path& plugin_path,
                           const char* format, const std::string& load_error) {
  if (!first) {
    json += ",";
  }
  first = false;
  const auto name = plugin_path.stem().string();
  const auto full_path = plugin_path.string();
  json += "{\"name\":\"" + escape_json(name) + "\",";
  json += "\"vendor\":\"Unknown Vendor\",";
  json += "\"category\":\"Uncategorized\",";
  json += "\"subCategories\":\"\",";
  json += "\"format\":\"" + std::string(format) + "\",";
  json += "\"path\":\"" + escape_json(full_path) + "\",";
  json += "\"modulePath\":\"" + escape_json(full_path) + "\",";
  json += "\"classId\":null,\"version\":\"\",\"sdkVersion\":\"\",";
  json += "\"loadError\":\"" +
          escape_json(load_error.empty() ? "Plug-in module could not be loaded"
                                         : load_error) +
          "\",";
  json += "\"isShellChild\":false,\"sdkMetadataLoaded\":false}";
}

/// Architecture sub-directory this build can load out of a VST3 bundle.
/// A bundle that only ships the other word size is not broken — it just is not
/// for this host, and reporting it as a load failure is more honest (and much
/// faster) than letting `LoadLibrary` fail on it.
constexpr const char* kHostArchDir =
#if defined(_WIN32)
#  if defined(_M_ARM64) || defined(__aarch64__)
    "arm64-win";
#  elif defined(_WIN64)
    "x86_64-win";
#  else
    "x86-win";
#  endif
#elif defined(__APPLE__)
    "MacOS";
#elif defined(__aarch64__)
    "aarch64-linux";
#else
    "x86_64-linux";
#endif

/// Empty when the bundle is loadable here. Otherwise a reason to report.
///
/// Only bundles that actually carry a `Contents/` directory are judged: the
/// legacy single-DLL `.vst3` layout has no architecture folders, and the loader
/// is left to decide.
std::string vst3_arch_mismatch_reason(const std::filesystem::path& bundle) {
  std::error_code ec;
  const auto contents = bundle / "Contents";
  if (!std::filesystem::is_directory(contents, ec)) {
    return {};
  }
  bool saw_arch_dir = false;
  std::string available;
  for (const auto& entry : std::filesystem::directory_iterator(contents, ec)) {
    if (ec) {
      return {};
    }
    if (!entry.is_directory(ec)) {
      continue;
    }
    const auto name = entry.path().filename().string();
    // `Resources` / `moduleinfo.json` siblings are not architecture folders.
    if (name == "Resources") {
      continue;
    }
    if (name == kHostArchDir) {
      return {};
    }
    saw_arch_dir = true;
    if (!available.empty()) {
      available += ", ";
    }
    available += name;
  }
  if (!saw_arch_dir) {
    return {};
  }
  return std::string("Plug-in has no ") + kHostArchDir +
         " build (found: " + available + ")";
}

std::string clap_feature_string(const char* const* features) {
  if (!features) {
    return "";
  }

  std::string joined;
  for (std::size_t i = 0; features[i]; ++i) {
    if (i > 0) {
      joined += "|";
    }
    joined += features[i];
  }
  return joined;
}

bool clap_has_feature(const char* const* features, const char* expected) {
  if (!features || !expected) {
    return false;
  }
  for (std::size_t i = 0; features[i]; ++i) {
    if (std::strcmp(features[i], expected) == 0) {
      return true;
    }
  }
  return false;
}

std::string clap_category(const char* const* features) {
  if (clap_has_feature(features, CLAP_PLUGIN_FEATURE_INSTRUMENT)) {
    return "Instrument";
  }
  if (clap_has_feature(features, CLAP_PLUGIN_FEATURE_AUDIO_EFFECT)) {
    return "Audio Effect";
  }
  if (clap_has_feature(features, CLAP_PLUGIN_FEATURE_NOTE_EFFECT)) {
    return "Note Effect";
  }
  if (features && features[0]) {
    return features[0];
  }
  return "Uncategorized";
}

class SharedLibrary {
 public:
  explicit SharedLibrary(const std::filesystem::path& path) {
#ifdef _WIN32
    handle_ = LoadLibraryW(path.wstring().c_str());
#else
    handle_ = dlopen(path.string().c_str(), RTLD_NOW | RTLD_LOCAL);
#endif
  }

  ~SharedLibrary() {
#ifdef _WIN32
    if (handle_) {
      FreeLibrary(static_cast<HMODULE>(handle_));
    }
#else
    if (handle_) {
      dlclose(handle_);
    }
#endif
  }

  SharedLibrary(const SharedLibrary&) = delete;
  SharedLibrary& operator=(const SharedLibrary&) = delete;

  bool valid() const { return handle_ != nullptr; }

  void* symbol(const char* name) const {
    if (!handle_) {
      return nullptr;
    }
#ifdef _WIN32
    return reinterpret_cast<void*>(GetProcAddress(static_cast<HMODULE>(handle_), name));
#else
    return dlsym(handle_, name);
#endif
  }

 private:
  void* handle_ = nullptr;
};

#ifdef __APPLE__
std::filesystem::path clap_executable_path(const std::filesystem::path& plugin_path) {
  if (!std::filesystem::is_directory(plugin_path)) {
    return plugin_path;
  }
  const auto name = plugin_path.stem().string();
  return plugin_path / "Contents" / "MacOS" / name;
}
#else
std::filesystem::path clap_executable_path(const std::filesystem::path& plugin_path) {
  return plugin_path;
}
#endif

} // namespace

extern "C" SpherePluginHostString sphere_vst3_scan_path_json(const char* path) {
  if (!path) {
    return make_string("[]");
  }

  const bool debug = std::getenv("SPHERE_PLUGIN_HOST_DEBUG") != nullptr;

  // Phase 1 host scanner: load VST3 factory metadata. Does not instantiate
  // processors, open editors, or touch realtime audio.
  std::filesystem::path root(path);
  if (!std::filesystem::exists(root)) {
    return make_string("[]");
  }

  std::string json = "[";
  bool first = true;

  const auto append = [&](const std::filesystem::path& plugin_path) {
    if (debug) {
      std::fprintf(stderr, "[SpherePluginHost] Scanning VST3: %s\n",
                   plugin_path.string().c_str());
    }

    const auto arch_reason = vst3_arch_mismatch_reason(plugin_path);
    if (!arch_reason.empty()) {
      if (debug) {
        std::fprintf(stderr, "[SpherePluginHost]   %s\n", arch_reason.c_str());
      }
      append_fallback_entry(json, first, plugin_path, "VST3", arch_reason);
      return;
    }

    std::string error;
    auto module = VST3::Hosting::Module::create(plugin_path.string(), error);
    if (!module) {
      if (debug) {
        std::fprintf(stderr, "[SpherePluginHost]   VST3 module load failed: %s\n",
                     error.c_str());
      }
      // Reported as a failure, not as a plug-in.
      append_fallback_entry(json, first, plugin_path, "VST3", error);
      return;
    }

    const auto factory = module->getFactory();
    Steinberg::Vst::HostApplication host_context;
    factory.setHostContext(&host_context);
    const auto factory_info = factory.info();
    const auto& raw_factory = factory.get();
    const auto raw_count = raw_factory ? raw_factory->countClasses() : 0;
    const auto class_count =
        raw_count > 0 ? static_cast<Steinberg::int32>(raw_count) : 0;

    if (debug) {
      std::fprintf(stderr, "[SpherePluginHost]   VST3 factory class count: %d\n",
                   class_count);
    }

    Steinberg::FUnknownPtr<Steinberg::IPluginFactory3> f3(raw_factory);
    Steinberg::FUnknownPtr<Steinberg::IPluginFactory2> f2(raw_factory);

    // Collect audio/plugin classes first so we can compute isShellChild.
    std::vector<ClassEntry> audio_classes;
    int skipped = 0;

    for (Steinberg::int32 i = 0; i < class_count; ++i) {
      std::string name, vendor, category, sub_categories, class_id, version,
          sdk_version;
      bool ok = false;

      Steinberg::PClassInfoW ci3{};
      Steinberg::PClassInfo2 ci2{};
      Steinberg::PClassInfo ci{};

      if (f3 && f3->getClassInfoUnicode(i, &ci3) == Steinberg::kResultTrue) {
        name = char_array_to_string(ci3.name, Steinberg::PClassInfo::kNameSize);
        vendor =
            char_array_to_string(ci3.vendor, Steinberg::PClassInfo2::kVendorSize);
        category = char_array_to_string(ci3.category,
                                        Steinberg::PClassInfo::kCategorySize);
        sub_categories = char_array_to_string(
            ci3.subCategories, Steinberg::PClassInfo2::kSubCategoriesSize);
        version = char_array_to_string(ci3.version,
                                       Steinberg::PClassInfo2::kVersionSize);
        sdk_version = char_array_to_string(ci3.sdkVersion,
                                           Steinberg::PClassInfo2::kVersionSize);
        class_id = uid_to_string(ci3.cid);
        ok = true;
      } else if (f2 &&
                 f2->getClassInfo2(i, &ci2) == Steinberg::kResultTrue) {
        name = char_array_to_string(ci2.name, Steinberg::PClassInfo::kNameSize);
        vendor =
            char_array_to_string(ci2.vendor, Steinberg::PClassInfo2::kVendorSize);
        category = char_array_to_string(ci2.category,
                                        Steinberg::PClassInfo::kCategorySize);
        sub_categories = char_array_to_string(
            ci2.subCategories, Steinberg::PClassInfo2::kSubCategoriesSize);
        version = char_array_to_string(ci2.version,
                                       Steinberg::PClassInfo2::kVersionSize);
        class_id = uid_to_string(ci2.cid);
        ok = true;
      } else if (raw_factory->getClassInfo(i, &ci) == Steinberg::kResultTrue) {
        name = char_array_to_string(ci.name, Steinberg::PClassInfo::kNameSize);
        category = char_array_to_string(ci.category,
                                        Steinberg::PClassInfo::kCategorySize);
        class_id = uid_to_string(ci.cid);
        ok = true;
      }

      if (debug) {
        std::fprintf(stderr,
                     "[SpherePluginHost]   class[%d]: name=%s category=%s\n",
                     i, name.c_str(), category.c_str());
      }

      if (!ok) {
        ++skipped;
        continue;
      }

      // Only VST3 audio module classes are user-visible plug-ins. Some vendors
      // also expose Plugin Compatibility or Controller classes from the same
      // module; listing those creates duplicate rows for one plug-in.
      if (category != kAudioModuleClass) {
        if (debug) {
          std::fprintf(stderr,
                       "[SpherePluginHost]     -> skipped (non-audio module class)\n");
        }
        ++skipped;
        continue;
      }

      if (vendor.empty()) {
        vendor = factory_info.vendor();
      }

      audio_classes.push_back(
          {name, vendor, category, sub_categories, class_id, version, sdk_version});
    }

    if (debug) {
      std::fprintf(stderr,
                   "[SpherePluginHost]   Accepted: %zu plugin classes, "
                   "skipped: %d\n",
                   audio_classes.size(), skipped);
    }

    // isShellChild: this module exposes more than one audio plugin class.
    const bool is_shell = (audio_classes.size() > 1);
    const std::string module_path = plugin_path.string();

    for (const auto& entry : audio_classes) {
      if (!first) {
        json += ",";
      }
      first = false;
      json += "{\"name\":\"" + escape_json(entry.name) + "\",";
      json += "\"vendor\":\"" + escape_json(entry.vendor) + "\",";
      json += "\"category\":\"" + escape_json(entry.category) + "\",";
      json += "\"subCategories\":\"" + escape_json(entry.sub_categories) + "\",";
      json += "\"format\":\"VST3\",";
      json += "\"path\":\"" + escape_json(module_path) + "\",";
      json += "\"modulePath\":\"" + escape_json(module_path) + "\",";
      json += "\"classId\":\"" + escape_json(entry.class_id) + "\",";
      json += "\"version\":\"" + escape_json(entry.version) + "\",";
      json += "\"sdkVersion\":\"" + escape_json(entry.sdk_version) + "\",";
      json += "\"isShellChild\":" +
              std::string(is_shell ? "true" : "false") + ",";
      json += "\"sdkMetadataLoaded\":true}";
    }
  };

  if (is_vst3_bundle(root)) {
    append(root);
  } else if (std::filesystem::is_directory(root)) {
    // A `.vst3` bundle is a leaf. `X.vst3/Contents/x86_64-win/X.vst3` matches
    // the same extension as the bundle directory itself, so a plain recursive
    // walk reported every bundle-format plug-in twice — under two different
    // paths, which produced two different stable ids that dedup could not
    // collapse. `disable_recursion_pending` prunes the subtree at the bundle.
    std::error_code ec;
    auto it = std::filesystem::recursive_directory_iterator(
        root, std::filesystem::directory_options::skip_permission_denied, ec);
    const std::filesystem::recursive_directory_iterator end;
    for (; !ec && it != end; it.increment(ec)) {
      if (is_vst3_bundle(it->path())) {
        append(it->path());
        it.disable_recursion_pending();
      }
    }
  }
  json += "]";
  return make_string(json);
}

extern "C" SpherePluginHostString sphere_clap_scan_path_json(const char* path) {
  if (!path) {
    return make_string("[]");
  }

  const bool debug = std::getenv("SPHERE_PLUGIN_HOST_DEBUG") != nullptr;
  std::filesystem::path root(path);
  if (!std::filesystem::exists(root)) {
    return make_string("[]");
  }

  std::string json = "[";
  bool first = true;

  const auto append = [&](const std::filesystem::path& plugin_path) {
    if (debug) {
      std::fprintf(stderr, "[SpherePluginHost] Scanning CLAP: %s\n",
                   plugin_path.string().c_str());
    }

    const auto executable_path = clap_executable_path(plugin_path);
    SharedLibrary library(executable_path);
    if (!library.valid()) {
      if (debug) {
        std::fprintf(stderr, "[SpherePluginHost]   CLAP module load failed\n");
      }
      append_fallback_entry(json, first, plugin_path, "CLAP",
                            "CLAP module could not be loaded (wrong "
                            "architecture or missing dependency)");
      return;
    }

    auto* entry = reinterpret_cast<const clap_plugin_entry_t*>(library.symbol("clap_entry"));
    if (!entry || !entry->init || !entry->deinit || !entry->get_factory) {
      if (debug) {
        std::fprintf(stderr, "[SpherePluginHost]   CLAP entry symbol invalid\n");
      }
      append_fallback_entry(json, first, plugin_path, "CLAP",
                            "CLAP entry point is missing or incomplete");
      return;
    }

    const auto path_string = plugin_path.string();
    if (!entry->init(path_string.c_str())) {
      if (debug) {
        std::fprintf(stderr, "[SpherePluginHost]   CLAP init failed\n");
      }
      append_fallback_entry(json, first, plugin_path, "CLAP",
                            "CLAP entry init() returned false");
      return;
    }

    const auto* factory = reinterpret_cast<const clap_plugin_factory_t*>(
        entry->get_factory(CLAP_PLUGIN_FACTORY_ID));
    if (!factory || !factory->get_plugin_count || !factory->get_plugin_descriptor) {
      entry->deinit();
      append_fallback_entry(json, first, plugin_path, "CLAP",
                            "CLAP plug-in factory is unavailable");
      return;
    }

    const uint32_t plugin_count = factory->get_plugin_count(factory);
    const bool is_shell = plugin_count > 1;
    for (uint32_t i = 0; i < plugin_count; ++i) {
      const auto* descriptor = factory->get_plugin_descriptor(factory, i);
      if (!descriptor) {
        continue;
      }

      const std::string name = descriptor->name ? descriptor->name : plugin_path.stem().string();
      const std::string vendor = descriptor->vendor ? descriptor->vendor : "Unknown Vendor";
      const std::string version = descriptor->version ? descriptor->version : "";
      const std::string class_id = descriptor->id ? descriptor->id : "";
      const std::string category = clap_category(descriptor->features);
      const std::string features = clap_feature_string(descriptor->features);

      if (!first) {
        json += ",";
      }
      first = false;
      json += "{\"name\":\"" + escape_json(name) + "\",";
      json += "\"vendor\":\"" + escape_json(vendor) + "\",";
      json += "\"category\":\"" + escape_json(category) + "\",";
      json += "\"subCategories\":\"" + escape_json(features) + "\",";
      json += "\"format\":\"CLAP\",";
      json += "\"path\":\"" + escape_json(path_string) + "\",";
      json += "\"modulePath\":\"" + escape_json(path_string) + "\",";
      if (class_id.empty()) {
        json += "\"classId\":null,";
      } else {
        json += "\"classId\":\"" + escape_json(class_id) + "\",";
      }
      json += "\"version\":\"" + escape_json(version) + "\",";
      json += "\"sdkVersion\":\"CLAP " +
              std::to_string(entry->clap_version.major) + "." +
              std::to_string(entry->clap_version.minor) + "." +
              std::to_string(entry->clap_version.revision) + "\",";
      json += "\"isShellChild\":" +
              std::string(is_shell ? "true" : "false") + ",";
      json += "\"sdkMetadataLoaded\":true}";
    }

    entry->deinit();
  };

  if (is_clap_plugin(root)) {
    append(root);
  } else if (std::filesystem::is_directory(root)) {
    // Same leaf rule as VST3: a macOS `.clap` bundle contains an executable of
    // the same name, so recursing into a matched bundle double-reports it.
    std::error_code ec;
    auto it = std::filesystem::recursive_directory_iterator(
        root, std::filesystem::directory_options::skip_permission_denied, ec);
    const std::filesystem::recursive_directory_iterator end;
    for (; !ec && it != end; it.increment(ec)) {
      if (is_clap_plugin(it->path())) {
        append(it->path());
        it.disable_recursion_pending();
      }
    }
  }

  json += "]";
  return make_string(json);
}

extern "C" void sphere_plugin_host_free_string(SpherePluginHostString value) {
  delete[] value.data;
}
