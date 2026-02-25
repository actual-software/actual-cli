import React from "react";
import { useCurrentFrame } from "remotion";

const BRAILLE = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

interface SpinnerProps {
  startFrame: number; // frame when spinner started
  color?: string;
}

export const Spinner: React.FC<SpinnerProps> = ({
  startFrame,
  color = "#e8f0ea",
}) => {
  const frame = useCurrentFrame();
  // 80ms per braille frame = 4.8 frames at 60fps
  const index = Math.floor((frame - startFrame) / 4.8) % BRAILLE.length;
  return (
    <span style={{ color, display: "inline-block", width: "1ch" }}>
      {BRAILLE[Math.abs(index)]}
    </span>
  );
};
