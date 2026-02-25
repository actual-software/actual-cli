import React from "react";
import { useCurrentFrame } from "remotion";
import { spring } from "remotion";
import { COLORS, FONTS } from "../../data/brand";
import { Spinner } from "./Spinner";

export type StepStatus =
  | "waiting"
  | "running"
  | "success"
  | "warning"
  | "error"
  | "skipped";

export interface StepDef {
  label: string;
  status: StepStatus;
  duration?: string;
  isActive?: boolean;
  spinnerStartFrame?: number;
  completionFrame?: number; // frame when step went to success/warning
}

const STATUS_ICONS: Record<StepStatus, string> = {
  waiting: "○",
  running: "", // handled by Spinner component
  success: "✔",
  warning: "⚠",
  error: "✖",
  skipped: "─",
};

const STATUS_COLORS: Record<StepStatus, string> = {
  waiting: COLORS.textDim,
  running: COLORS.borderGreen,
  success: COLORS.borderGreen,
  warning: COLORS.warning,
  error: COLORS.error,
  skipped: COLORS.textDim,
};

interface StepRowProps {
  step: StepDef;
  isActive: boolean;
}

const StepRow: React.FC<StepRowProps> = ({ step, isActive }) => {
  const frame = useCurrentFrame();

  // Glow on completion
  const glowValue =
    step.completionFrame != null
      ? spring({
          frame: frame - step.completionFrame,
          fps: 60,
          config: { mass: 0.4, damping: 8 },
          durationInFrames: 30,
        })
      : 0;
  const glow = Math.min(glowValue, 1.3); // clamp overshoot

  const color = STATUS_COLORS[step.status];
  const iconColor =
    step.status === "success"
      ? `rgba(0,251,126,${0.6 + glow * 0.4})`
      : color;

  return (
    <div
      style={{
        fontFamily: FONTS.mono,
        fontSize: 13,
        lineHeight: 1.8,
        display: "flex",
        alignItems: "center",
        gap: 6,
        padding: "0 10px",
        color:
          step.status === "waiting" ? COLORS.textDim : COLORS.textPrimary,
        filter:
          glow > 0.1
            ? `drop-shadow(0 0 ${glow * 8}px ${COLORS.borderGreen})`
            : "none",
      }}
    >
      {/* Active marker */}
      <span style={{ width: "1ch", color: COLORS.borderGreen }}>
        {isActive ? "▶" : " "}
      </span>

      {/* Status icon */}
      <span
        style={{ color: iconColor, width: "1ch", display: "inline-block" }}
      >
        {step.status === "running" && step.spinnerStartFrame != null ? (
          <Spinner startFrame={step.spinnerStartFrame} color={color} />
        ) : (
          STATUS_ICONS[step.status]
        )}
      </span>

      {/* Label */}
      <span style={{ flexGrow: 1 }}>{step.label}</span>

      {/* Duration */}
      {step.duration && (
        <span style={{ color: COLORS.textDim, fontSize: 11 }}>
          [{step.duration}]
        </span>
      )}
    </div>
  );
};

interface StepsPanelProps {
  steps: StepDef[];
  activeStepIndex: number;
}

export const StepsPanel: React.FC<StepsPanelProps> = ({
  steps,
  activeStepIndex,
}) => (
  <div
    style={{
      borderTop: `1px solid ${COLORS.borderTeal}44`,
      paddingTop: 4,
      paddingBottom: 4,
    }}
  >
    {steps.map((step, i) => (
      <StepRow key={step.label} step={step} isActive={i === activeStepIndex} />
    ))}
  </div>
);
