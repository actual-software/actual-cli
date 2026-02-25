import React from "react";
import { Composition } from "remotion";
import { TestFrame } from "./compositions/TestFrame";
import { TuiPreview } from "./compositions/TuiPreview";
import { PipelinePreview } from "./compositions/PipelinePreview";

export const Root: React.FC = () => {
  return (
    <>
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
    </>
  );
};
