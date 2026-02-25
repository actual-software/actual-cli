import React from "react";
import { useCurrentFrame } from "remotion";

const BRAILLE = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

interface SpinnerProps {
  startFrame: number; // frame when spinner started
  color?: string;
  currentFrame?: number; // override useCurrentFrame() for remapped clips
}

export const Spinner: React.FC<SpinnerProps> = ({
  startFrame,
  color = "#e8f0ea",
  currentFrame,
}) => {
  const remotionFrame = useCurrentFrame();
  const frame = currentFrame ?? remotionFrame;
  // 80ms per braille frame = 4.8 frames at 60fps
  const index = Math.floor((frame - startFrame) / 4.8) % BRAILLE.length;
  return (
    <span style={{ color, display: "inline-block", width: "1ch" }}>
      {BRAILLE[Math.abs(index)]}
    </span>
  );
};
