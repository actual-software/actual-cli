import React from "react";
import { Composition } from "remotion";
import { TestFrame } from "./compositions/TestFrame";
import { TuiPreview } from "./compositions/TuiPreview";
import { PipelinePreview } from "./compositions/PipelinePreview";
import { HeroClip } from "./components/scenes/HeroClip";
import { ShortClip } from "./components/scenes/ShortClip";
import { LoopClip } from "./components/scenes/LoopClip";

export const Root: React.FC = () => {
  return (
    <>
      {/* Dev/test compositions */}
      <Composition
        id="TestFrame"
        component={TestFrame}
        durationInFrames={60}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />
      <Composition
        id="TuiPreview"
        component={TuiPreview}
        durationInFrames={120}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />
      <Composition
        id="PipelinePreview"
        component={PipelinePreview}
        durationInFrames={1440}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />

      {/* Production compositions */}
      <Composition
        id="HeroClip"
        component={HeroClip}
        durationInFrames={1800}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />

      {/* Short clip — 3 aspect ratios (all use the same component, different dimensions) */}
      <Composition
        id="ShortClip-169"
        component={ShortClip}
        durationInFrames={900}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />
      <Composition
        id="ShortClip-11"
        component={ShortClip}
        durationInFrames={900}
        fps={60}
        width={1080}
        height={1080}
        defaultProps={{}}
      />
      <Composition
        id="ShortClip-916"
        component={ShortClip}
        durationInFrames={900}
        fps={60}
        width={1080}
        height={1920}
        defaultProps={{}}
      />

      {/* README loop — 10s seamless pipeline loop */}
      <Composition
        id="LoopClip"
        component={LoopClip}
        durationInFrames={600}
        fps={60}
        width={1200}
        height={680}
        defaultProps={{}}
      />
    </>
  );
};
