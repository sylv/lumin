import TorrentItem from "../../components/torrent-item.jsx";
import { trpc } from "../trpc.js";

export default function Page() {
	const [torrents] = trpc.get_torrents.useSuspenseQuery({} as any, {
		refetchInterval: 5000,
	});

	return (
		<div className="m-4">
			<h1 className="text-2xl font-bold mb-4">Active Torrents</h1>
			{torrents.map((torrent) => (
				<TorrentItem key={torrent.id} torrent={torrent} />
			))}
		</div>
	);
}
