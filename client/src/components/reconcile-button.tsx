import { trpc } from "../pages/trpc";
import { Button } from "./ui/button";
import { RefreshCw, Loader2 } from "lucide-react";

export default function ReconcileButton() {
	const reconcileMutation = trpc.reconcile_torrents.useMutation();

	const handleReconcile = () => {
		reconcileMutation.mutate();
	};

	return (
		<Button
			variant="outline"
			onClick={handleReconcile}
			disabled={reconcileMutation.isPending}
		>
			{reconcileMutation.isPending ? (
				<Loader2 className="h-4 w-4 mr-2 animate-spin" />
			) : (
				<RefreshCw className="h-4 w-4 mr-2" />
			)}
			Reconcile
		</Button>
	);
}
