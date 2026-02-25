import React from "react";
import { Sequence } from "remotion";
import { FRAMES } from "../../data/tui-states";
import { SceneHook } from "./SceneHook";
import { SceneTuiReveal } from "./SceneTuiReveal";
import { ScenePipeline } from "./ScenePipeline";
import { SceneComplete } from "./SceneComplete";
import { SceneCta } from "./SceneCta";
import { FilmGrain } from "../effects/FilmGrain";
import { Vignette } from "../effects/Vignette";

export const HeroClip: React.FC = () => {
  return (
    <div style={{ position: "relative", width: 1920, height: 1080 }}>
      {/* Scenes */}
      <Sequence from={0} durationInFrames={FRAMES.HOOK_END}>
        <SceneHook />
      </Sequence>
      <Sequence
        from={FRAMES.HOOK_END}
        durationInFrames={FRAMES.REVEAL_END - FRAMES.HOOK_END}
      >
        <SceneTuiReveal />
      </Sequence>
      {/* Pipeline + Summary step (REVEAL_END → COMPLETE_START) */}
      <Sequence
        from={FRAMES.REVEAL_END}
        durationInFrames={FRAMES.COMPLETE_START - FRAMES.REVEAL_END}
      >
        <ScenePipeline />
      </Sequence>
      <Sequence
        from={FRAMES.COMPLETE_START}
        durationInFrames={FRAMES.CTA_START - FRAMES.COMPLETE_START}
      >
        <SceneComplete />
      </Sequence>
      {/* +180f (3s) hold on the CTA wordmark/tagline */}
      <Sequence
        from={FRAMES.CTA_START}
        durationInFrames={FRAMES.CLIP_END - FRAMES.CTA_START + 180}
      >
        <SceneCta totalDuration={FRAMES.CLIP_END - FRAMES.CTA_START + 180} />
      </Sequence>

      {/* Composition-level overlays */}
      <FilmGrain width={1920} height={1080} opacity={0.035} />
      <Vignette intensity={0.55} />
    </div>
  );
};
