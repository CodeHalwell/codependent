import { useControlPlaneContext } from "./useControlPlaneContext.js";
import type { ControlPlaneClient, ControlPlaneStreamClient } from "@codypendent/control-plane";

export interface UseControlPlaneClientResult {
  client: ControlPlaneClient;
  streamClient: ControlPlaneStreamClient;
}

export function useControlPlaneClient(): UseControlPlaneClientResult {
  const { client, streamClient } = useControlPlaneContext();
  return { client, streamClient };
}
