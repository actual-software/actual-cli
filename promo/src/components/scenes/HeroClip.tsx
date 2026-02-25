import React from "react";
import { Sequence } from "remotion";
import { FRAMES } from "../../data/tui-states";
import { SceneHook } from "./SceneHook";
import { SceneTuiReveal } from "./SceneTuiReveal";
import { ScenePipeline } from "./ScenePipeline";
import { SceneComplete } from "./SceneComplete";
import { SceneCta } from "./SceneCta";

export const HeroClip: React.FC = () => {
  return (
    <>
      {/* Scene 1: Hook (0–180) */}
      <Sequence from={0} durationInFrames={FRAMES.HOOK_END}>
        <SceneHook />
      </Sequence>

      {/* Scene 2: TUI Reveal (180–360) */}
      <Sequence
        from={FRAMES.HOOK_END}
        durationInFrames={FRAMES.REVEAL_END - FRAMES.HOOK_END}
      >
        <SceneTuiReveal />
      </Sequence>

      {/* Scene 3: Pipeline (360–1320) */}
      <Sequence
        from={FRAMES.REVEAL_END}
        durationInFrames={FRAMES.WRITE_END - FRAMES.REVEAL_END}
      >
        <ScenePipeline />
      </Sequence>

      {/* Scene 4: Complete (1320–1620) */}
      <Sequence
        from={FRAMES.WRITE_END}
        durationInFrames={FRAMES.CTA_START - FRAMES.WRITE_END}
      >
        <SceneComplete />
      </Sequence>

      {/* Scene 5: CTA (1620–1800) */}
      <Sequence
        from={FRAMES.CTA_START}
        durationInFrames={FRAMES.CLIP_END - FRAMES.CTA_START}
      >
        <SceneCta />
      </Sequence>
    </>
  );
};
