import React from "react";
import { Composition } from "remotion";
import { TestFrame } from "./compositions/TestFrame";
import { TuiPreview } from "./compositions/TuiPreview";
import { PipelinePreview } from "./compositions/PipelinePreview";
import { HeroClip } from "./components/scenes/HeroClip";
import { SocialMediaClip } from "./components/scenes/SocialMediaClip";
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
        durationInFrames={1170}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />

      {/* Social media clip — 3 aspect ratios (all use the same component, different dimensions) */}
      <Composition
        id="HorizontalFullClip"
        component={SocialMediaClip}
        durationInFrames={1080}
        fps={60}
        width={1920}
        height={1080}
        defaultProps={{}}
      />
      <Composition
        id="SocialMediaSquareClip"
        component={SocialMediaClip}
        durationInFrames={1080}
        fps={60}
        width={1080}
        height={1080}
        defaultProps={{}}
      />
      <Composition
        id="SocialMediaVerticalClip"
        component={SocialMediaClip}
        durationInFrames={1080}
        fps={60}
        width={1080}
        height={1920}
        defaultProps={{}}
      />

      {/* README loop — 12s seamless pipeline loop (10s pipeline + 2s hold) */}
      <Composition
        id="LoopClip"
        component={LoopClip}
        durationInFrames={720}
        fps={60}
        width={1200}
        height={680}
        defaultProps={{}}
      />
    </>
  );
};
