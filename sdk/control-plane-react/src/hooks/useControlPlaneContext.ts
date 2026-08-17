import { useContext } from "react";
import { ControlPlaneContext, type ControlPlaneContextValue } from "../context.js";

export function useControlPlaneContext(): ControlPlaneContextValue {
  const context = useContext(ControlPlaneContext);
  if (!context) {
    throw new Error("useControlPlaneContext must be used within a ControlPlaneProvider");
  }
  return context;
}
