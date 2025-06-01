import TorrentItem from "../../components/torrent-item.jsx";
import AddTorrentDialog from "../../components/add-torrent-dialog.jsx";
import ReconcileButton from "../../components/reconcile-button.jsx";
import { trpc } from "../trpc.js";

export default function Page() {
	const [torrents] = trpc.get_torrents.useSuspenseQuery({} as any, {
		refetchInterval: 5000,
	});

	return (
		<div className="m-4">
			<div className="flex items-center justify-between mb-4">
				<h1 className="text-2xl font-bold">Active Torrents</h1>
				<div className="flex gap-2">
					<ReconcileButton />
					<AddTorrentDialog />
				</div>
			</div>
			{torrents.length > 0 ? (
				torrents.map((torrent) => (
					<TorrentItem key={torrent.id} torrent={torrent} />
				))
			) : (
				<div className="text-center py-8 text-muted-foreground">
					<p>No torrents found.</p>
					<p className="text-sm mt-1">Add a torrent to get started!</p>
				</div>
			)}
		</div>
	);
}
