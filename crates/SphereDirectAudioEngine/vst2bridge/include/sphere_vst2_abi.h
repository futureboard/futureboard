#pragma once

// Minimal VST 2.4 host-side ABI declarations.
//
// The Steinberg VST2 SDK is discontinued and not redistributable, so this
// header declares only the published binary interface a *host* needs to talk to
// an existing VST2 module. No Steinberg source is vendored, and nothing here
// is used to *build* a VST2 plug-in.
//
// Layout is fixed by the ABI — do not reorder, resize, or "clean up" any field
// in `AEffect`, `VstEvents`, `VstMidiEvent`, or `VstTimeInfo`. Reserved padding
// members exist because real plug-ins index past the documented fields.

#include <cstdint>

#if defined(_WIN32)
#define SPHERE_VST2_CC __cdecl
#else
#define SPHERE_VST2_CC
#endif

extern "C" {

struct AEffect;

using AEffectDispatcherProc = intptr_t(SPHERE_VST2_CC *)(AEffect *effect,
                                                         int32_t opcode,
                                                         int32_t index,
                                                         intptr_t value,
                                                         void *ptr,
                                                         float opt);
using AEffectProcessProc = void(SPHERE_VST2_CC *)(AEffect *effect,
                                                  float **inputs,
                                                  float **outputs,
                                                  int32_t sample_frames);
using AEffectProcessDoubleProc = void(SPHERE_VST2_CC *)(AEffect *effect,
                                                        double **inputs,
                                                        double **outputs,
                                                        int32_t sample_frames);
using AEffectSetParameterProc = void(SPHERE_VST2_CC *)(AEffect *effect,
                                                       int32_t index,
                                                       float parameter);
using AEffectGetParameterProc = float(SPHERE_VST2_CC *)(AEffect *effect,
                                                        int32_t index);

using AudioMasterCallback = intptr_t(SPHERE_VST2_CC *)(AEffect *effect,
                                                       int32_t opcode,
                                                       int32_t index,
                                                       intptr_t value,
                                                       void *ptr, float opt);

/// Entry point signature. Modern modules export `VSTPluginMain`; pre-2.4
/// modules export `main` (and `main_macho` on old macOS bundles).
using VstPluginMainProc = AEffect *(SPHERE_VST2_CC *)(AudioMasterCallback host);

constexpr int32_t kEffectMagic = 0x56737450; // 'VstP'

struct AEffect {
  int32_t magic;
  AEffectDispatcherProc dispatcher;
  /// Deprecated accumulating process. Never called by this host — VST 2.4
  /// requires `processReplacing`.
  AEffectProcessProc process_deprecated;
  AEffectSetParameterProc setParameter;
  AEffectGetParameterProc getParameter;

  int32_t numPrograms;
  int32_t numParams;
  int32_t numInputs;
  int32_t numOutputs;

  int32_t flags;

  intptr_t resvd1;
  intptr_t resvd2;

  int32_t initialDelay;

  int32_t realQualities_deprecated;
  int32_t offQualities_deprecated;
  float ioRatio_deprecated;

  void *object;
  void *user;

  int32_t uniqueID;
  int32_t version;

  AEffectProcessProc processReplacing;
  AEffectProcessDoubleProc processDoubleReplacing;

  char future[56];
};

// ── AEffect::flags ──────────────────────────────────────────────────────────
enum AEffectFlags : int32_t {
  effFlagsHasEditor = 1 << 0,
  effFlagsCanReplacing = 1 << 4,
  effFlagsProgramChunks = 1 << 5,
  effFlagsIsSynth = 1 << 8,
  effFlagsNoSoundInStop = 1 << 9,
  effFlagsCanDoubleReplacing = 1 << 12,
};

// ── Host → plug-in opcodes (effXxx) ─────────────────────────────────────────
enum AEffectOpcodes : int32_t {
  effOpen = 0,
  effClose = 1,
  effSetProgram = 2,
  effGetProgram = 3,
  effSetProgramName = 4,
  effGetProgramName = 5,
  effGetParamLabel = 6,
  effGetParamDisplay = 7,
  effGetParamName = 8,
  effSetSampleRate = 10,
  effSetBlockSize = 11,
  effMainsChanged = 12,
  effEditGetRect = 13,
  effEditOpen = 14,
  effEditClose = 15,
  effEditIdle = 19,
  effGetChunk = 23,
  effSetChunk = 24,

  effProcessEvents = 25,
  effCanBeAutomated = 26,
  effString2Parameter = 27,
  effGetProgramNameIndexed = 29,
  effGetInputProperties = 33,
  effGetOutputProperties = 34,
  effGetPlugCategory = 35,
  effSetSpeakerArrangement = 42,
  effGetEffectName = 45,
  effGetVendorString = 47,
  effGetProductString = 48,
  effGetVendorVersion = 49,
  effCanDo = 51,
  effGetTailSize = 52,
  effGetParameterProperties = 56,
  effGetVstVersion = 58,
  effStartProcess = 71,
  effStopProcess = 72,
  effSetProcessPrecision = 77,
  effGetNumMidiInputChannels = 78,
  effGetNumMidiOutputChannels = 79,

  /// Shell modules (Waves, Kontakt): fills `ptr` with the sub-plug-in name and
  /// returns its uniqueID, or 0 when the enumeration is exhausted.
  effShellGetNextPlugin = 70,
};

// ── Plug-in → host opcodes (audioMasterXxx) ─────────────────────────────────
enum AudioMasterOpcodes : int32_t {
  audioMasterAutomate = 0,
  audioMasterVersion = 1,
  audioMasterCurrentId = 2,
  audioMasterIdle = 3,
  audioMasterGetTime = 7,
  audioMasterProcessEvents = 8,
  audioMasterIOChanged = 13,
  audioMasterSizeWindow = 15,
  audioMasterGetSampleRate = 16,
  audioMasterGetBlockSize = 17,
  audioMasterGetInputLatency = 18,
  audioMasterGetOutputLatency = 19,
  audioMasterGetCurrentProcessLevel = 23,
  audioMasterGetAutomationState = 24,
  audioMasterGetVendorString = 32,
  audioMasterGetProductString = 33,
  audioMasterGetVendorVersion = 34,
  audioMasterCanDo = 37,
  audioMasterGetLanguage = 38,
  audioMasterUpdateDisplay = 42,
  audioMasterBeginEdit = 43,
  audioMasterEndEdit = 44,
  audioMasterOpenFileSelector = 45,
  audioMasterCloseFileSelector = 46,
};

enum VstProcessLevels : int32_t {
  kVstProcessLevelUnknown = 0,
  kVstProcessLevelUser = 1,
  kVstProcessLevelRealtime = 2,
  kVstProcessLevelOffline = 4,
};

enum VstProcessPrecision : int32_t {
  kVstProcessPrecision32 = 0,
  kVstProcessPrecision64 = 1,
};

enum VstPlugCategory : int32_t {
  kPlugCategUnknown = 0,
  kPlugCategEffect = 1,
  kPlugCategSynth = 2,
  kPlugCategAnalysis = 3,
  kPlugCategMastering = 4,
  kPlugCategSpacializer = 5,
  kPlugCategRoomFx = 6,
  kPlugSurroundFx = 7,
  kPlugCategRestoration = 8,
  kPlugCategOfflineProcess = 9,
  kPlugCategShell = 10,
  kPlugCategGenerator = 11,
};

// ── Events ──────────────────────────────────────────────────────────────────
enum VstEventTypes : int32_t {
  kVstMidiType = 1,
  kVstSysExType = 6,
};

struct VstEvent {
  int32_t type;
  int32_t byteSize;
  int32_t deltaFrames;
  int32_t flags;
  char data[16];
};

enum VstMidiEventFlags : int32_t {
  kVstMidiEventIsRealtime = 1 << 0,
};

struct VstMidiEvent {
  int32_t type;        // kVstMidiType
  int32_t byteSize;    // sizeof(VstMidiEvent)
  int32_t deltaFrames; // sample offset within the block
  int32_t flags;
  int32_t noteLength;
  int32_t noteOffset;
  char midiData[4];
  char detune;
  char noteOffVelocity;
  char reserved1;
  char reserved2;
};

/// Trailing `events[]` is a flexible array in the ABI; callers allocate a
/// larger block and index past `events[1]`.
struct VstEvents {
  int32_t numEvents;
  intptr_t reserved;
  VstEvent *events[1];
};

// ── Transport ───────────────────────────────────────────────────────────────
enum VstTimeInfoFlags : int32_t {
  kVstTransportChanged = 1 << 0,
  kVstTransportPlaying = 1 << 1,
  kVstTransportCycleActive = 1 << 2,
  kVstTransportRecording = 1 << 3,
  kVstAutomationWriting = 1 << 6,
  kVstAutomationReading = 1 << 7,
  kVstNanosValid = 1 << 8,
  kVstPpqPosValid = 1 << 9,
  kVstTempoValid = 1 << 10,
  kVstBarsValid = 1 << 11,
  kVstCyclePosValid = 1 << 12,
  kVstTimeSigValid = 1 << 13,
  kVstSmpteValid = 1 << 14,
  kVstClockValid = 1 << 15,
};

struct VstTimeInfo {
  double samplePos;
  double sampleRate;
  double nanoSeconds;
  double ppqPos;
  double tempo;
  double barStartPos;
  double cycleStartPos;
  double cycleEndPos;
  int32_t timeSigNumerator;
  int32_t timeSigDenominator;
  int32_t smpteOffset;
  int32_t smpteFrameRate;
  int32_t samplesToNextClock;
  int32_t flags;
};

struct ERect {
  int16_t top;
  int16_t left;
  int16_t bottom;
  int16_t right;
};

// ── Parameter / pin properties ──────────────────────────────────────────────
enum VstParameterFlags : int32_t {
  kVstParameterIsSwitch = 1 << 0,
  kVstParameterUsesIntegerMinMax = 1 << 1,
  kVstParameterUsesFloatStep = 1 << 2,
  kVstParameterUsesIntStep = 1 << 3,
  kVstParameterSupportsDisplayIndex = 1 << 4,
  kVstParameterSupportsDisplayCategory = 1 << 5,
  kVstParameterCanRamp = 1 << 6,
};

struct VstParameterProperties {
  float stepFloat;
  float smallStepFloat;
  float largeStepFloat;
  char label[64];
  int32_t flags;
  int32_t minInteger;
  int32_t maxInteger;
  int32_t stepInteger;
  int32_t largeStepInteger;
  char shortLabel[8];
  int16_t displayIndex;
  int16_t category;
  int16_t numParametersInCategory;
  int16_t reserved;
  char categoryLabel[24];
  char future[16];
};

enum VstPinPropertiesFlags : int32_t {
  kVstPinIsActive = 1 << 0,
  kVstPinIsStereo = 1 << 1,
  kVstPinUseSpeaker = 1 << 2,
};

struct VstPinProperties {
  char label[64];
  int32_t flags;
  int32_t arrangementType;
  char shortLabel[8];
  char future[48];
};

} // extern "C"
