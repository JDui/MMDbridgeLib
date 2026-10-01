import { Button, Group, Modal, Stack, Text, TextInput } from "@mantine/core";
import { useEffect, useRef, useState } from "react";

type DialogRequest = { message: string; value: string; prompt: boolean };

// Keep the existing operation flow asynchronous; cancellation never submits an operation.
export function useLibraryDialog() {
  const [request, setRequest] = useState<DialogRequest | null>(null);
  const resolveRef = useRef<((value: string | null) => void) | null>(null);
  useEffect(() => () => { resolveRef.current?.(null); resolveRef.current = null; }, []);
  function finish(value: string | null) {
    resolveRef.current?.(value);
    resolveRef.current = null;
    setRequest(null);
  }
  function ask(message: string, value: string, prompt: boolean) {
    resolveRef.current?.(null);
    setRequest({ message, value, prompt });
    return new Promise<string | null>((resolve) => { resolveRef.current = resolve; });
  }
  return {
    prompt: (message: string, value = "") => ask(message, value, true),
    confirm: async (message: string) => (await ask(message, "", false)) !== null,
    dialog: <Modal opened={request !== null} onClose={() => finish(null)} title={request?.prompt ? "输入信息" : "确认操作"} zIndex={400} size="md" closeButtonProps={{ "aria-label": "取消操作" }}>
      {request && <form onSubmit={(event) => { event.preventDefault(); finish(request.value); }}>
        <Stack gap="md">
          <Text size="sm" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>{request.message}</Text>
          {request.prompt && <TextInput aria-label={request.message} value={request.value} data-autofocus onChange={(event) => setRequest({ ...request, value: event.currentTarget.value })} />}
          <Group justify="flex-end"><Button variant="default" onClick={() => finish(null)}>取消</Button><Button type="submit" variant="filled" data-autofocus={!request.prompt || undefined}>确认</Button></Group>
        </Stack>
      </form>}
    </Modal>,
  };
}
