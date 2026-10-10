import { type ReactNode, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { messageOf } from "@/features/import/errors";
import type { RecoverySnapshot } from "@/ipc/bindings/RecoverySnapshot";
import { projectRecovery, projectRecoveryClear } from "@/ipc/commands";
import { Button, Dialog, toast } from "@/ui";

/** When the snapshot was written, in the user's locale. Falls back to the raw text when it does not parse. */
function formatSavedAt(savedAt: string): string {
  const date = new Date(savedAt);
  return Number.isNaN(date.getTime()) ? savedAt : date.toLocaleString();
}

/**
 * Looks for a recovery snapshot once, at startup. When one exists the returned node is the "Recover unsaved
 * project?" dialog. Recover goes through `restore` (the import flow, which handles network shares and missing
 * media); Discard clears the slot.
 */
export function useRecoveryPrompt(restore: (snapshot: RecoverySnapshot) => void): ReactNode {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState<RecoverySnapshot | null>(null);
  const checked = useRef(false);

  useEffect(() => {
    if (checked.current) return;
    checked.current = true;
    projectRecovery()
      .then((found) => {
        if (found) setSnapshot(found);
      })
      .catch((error: unknown) => {
        console.warn("LoopCut recovery check failed:", error);
      });
  }, []);

  if (!snapshot) return null;

  const close = () => setSnapshot(null);

  const discard = () => {
    close();
    projectRecoveryClear().catch((error: unknown) => {
      toast({
        variant: "error",
        title: t("project.recoveryClearFailed"),
        description: messageOf(error),
      });
    });
  };

  const recover = () => {
    close();
    restore(snapshot);
  };

  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open) close();
      }}
      size="sm"
      title={t("project.recoveryTitle")}
      description={t("project.recoveryBody", {
        name: snapshot.project.name,
        time: formatSavedAt(snapshot.savedAt),
      })}
      footer={
        <>
          <Button variant="secondary" onClick={discard}>
            {t("project.recoveryDiscard")}
          </Button>
          <Button variant="primary" onClick={recover}>
            {t("project.recoveryRecover")}
          </Button>
        </>
      }
    />
  );
}
