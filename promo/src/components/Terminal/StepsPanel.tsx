import React from "react";
import { useCurrentFrame } from "remotion";
import { spring } from "remotion";
import { COLORS, FONTS } from "../../data/brand";
import { Spinner } from "./Spinner";
import { GlowWrapper } from "../effects/GlowWrapper";

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

// Matches real TUI: ✓ (U+2713) for success, not ✔ (U+2714 heavy)
const STATUS_ICONS: Record<StepStatus, string> = {
  waiting: "○",
  running: "", // handled by Spinner component
  success: "✓",
  warning: "⚠",
  error: "✖",
  skipped: "-",
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
}

const StepRow: React.FC<StepRowProps> = ({ step }) => {
  const frame = useCurrentFrame();

  // Spring glow burst on step completion
  const glowValue =
    step.completionFrame != null
      ? spring({
          frame: frame - step.completionFrame,
          fps: 60,
          config: { mass: 0.4, damping: 8 },
          durationInFrames: 30,
        })
      : 0;
  const glow = Math.min(glowValue, 1.3);

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
        lineHeight: 1.7,
        display: "flex",
        alignItems: "center",
        padding: "1px 12px",
        color: step.status === "waiting" ? COLORS.textDim : COLORS.textPrimary,
      }}
    >
      {/* Status icon — no ▶ active marker (matches real TUI) */}
      <GlowWrapper intensity={glow} color={COLORS.borderGreen} maxRadius={12}>
        <span
          style={{
            color: iconColor,
            width: "1.2ch",
            display: "inline-block",
            marginRight: 6,
          }}
        >
          {step.status === "running" && step.spinnerStartFrame != null ? (
            <Spinner startFrame={step.spinnerStartFrame} color={color} />
          ) : (
            STATUS_ICONS[step.status]
          )}
        </span>
      </GlowWrapper>

      {/* Label — grows to push duration right */}
      <span style={{ flexGrow: 1 }}>{step.label}</span>

      {/* Duration — right-aligned, dim, matches real TUI [0.0s] format */}
      {step.duration && (
        <span style={{ color: COLORS.textDim, fontSize: 12, marginLeft: 8 }}>
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
  // activeStepIndex kept in interface for API compatibility but not rendered
  // (real TUI doesn't show a ▶ marker — the running spinner is sufficient)
}) => (
  <div style={{ paddingTop: 6, paddingBottom: 6 }}>
    {steps.map((step) => (
      <StepRow key={step.label} step={step} />
    ))}
  </div>
);
