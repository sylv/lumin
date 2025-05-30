import { useState } from "react";
import { trpc } from "../pages/trpc";
import type { Torrent, TorrentFile } from "../@generated/server";
import {
	ChevronDown,
	ChevronRight,
	Download,
	Upload,
	Users,
	CheckCircle,
	XCircle,
	AlertTriangle,
	Hourglass,
	Trash2,
	DownloadIcon,
	CircleFadingArrowUpIcon,
	BlendIcon,
	ShareIcon,
	Share2Icon,
	CircleIcon,
	RadiusIcon,
} from "lucide-react";
import clsx from "clsx";

interface TorrentItemProps {
	torrent: Torrent;
}

function formatBytes(bytes: number, decimals = 2) {
	if (bytes === 0) return "0B";
	const k = 1024;
	const dm = decimals < 0 ? 0 : decimals;
	const sizes = ["B", "KB", "MB", "GB", "TB", "PB", "EB", "ZB", "YB"];
	const i = Math.floor(Math.log(bytes) / Math.log(k));
	return `${Number.parseFloat((bytes / k ** i).toFixed(dm))}${sizes[i]}`;
}

function formatSpeed(bytes: number) {
	return `${formatBytes(bytes)}/s`;
}

function getStatusColour(state: Torrent["state"]) {
	switch (state) {
		case "Ready":
			return "bg-emerald-400/20 text-emerald-400 border-emerald-400/30";
		case "Pending":
		case "Downloading":
			return "bg-indigo-400/20 text-indigo-400 border-indigo-400/30";
		case "Stalled":
			return "bg-amber-400/20 text-amber-400 border-amber-400/30";
		case "Removing":
		case "Error":
			return "bg-red-400/20 text-red-400 border-red-400/30";
	}
}

export default function TorrentItem({ torrent }: TorrentItemProps) {
	const [isExpanded, setIsExpanded] = useState(false);
	const {
		data: files,
		isLoading: filesLoading,
		error: filesError,
	} = trpc.get_torrent_files.useQuery(
		{ torrent_id: torrent.id },
		{ enabled: isExpanded },
	);

	const utils = trpc.useUtils();
	const toggleExpand = () => setIsExpanded(!isExpanded);

	return (
		<div className="border mb-2">
			<button
				type="button"
				className="cursor-pointer w-full text-left p-3 hover:bg-zinc-900/50"
				onClick={toggleExpand}
			>
				<div className="flex gap-2 flex-col">
					<div className="flex items-center gap-2">
						<span
							className={clsx(
								"font-mono text-sm lowercase border px-2",
								getStatusColour(torrent.state),
							)}
						>
							{torrent.state}
						</span>
						<span className="font-semibold">{torrent.name}</span>
					</div>
					<div className="flex items-center gap-4 text-xs text-zinc-400">
						<div className="flex items-center gap-1" title="Torrent size">
							<DownloadIcon className="h-3.5 w-3.5" />
							{(torrent.progress * 100).toFixed(2)}% of{" "}
							{formatBytes(torrent.size)}
						</div>
						<div className="flex items-center gap-1" title="Ratio">
							<CircleFadingArrowUpIcon className="h-3.5 w-3.5" />
							{torrent.ratio.toFixed(2)}
						</div>
						<div className="flex items-center gap-1" title="Peers">
							<BlendIcon className="h-3.5 w-3.5" />
							{torrent.peers.toLocaleString()} peers
						</div>
						<div className="flex items-center gap-1" title="Seeds">
							<RadiusIcon className="h-3.5 w-3.5" />
							{torrent.seeds.toLocaleString()} seeds
						</div>
					</div>
				</div>
			</button>
			{isExpanded && (
				<div className="p-3 border-t border-border-color bg-background-secondary">
					{filesLoading && <p>Loading files...</p>}
					{filesError && (
						<p className="text-red-400">{`Error loading files: ${filesError.message}`}</p>
					)}
					{files && files.length > 0 && (
						<ul className="space-y-1">
							{files.map((file) => (
								<li
									key={file.id}
									className="text-sm p-1 hover:bg-card-hover-muted truncate"
									title={file.path}
								>
									{file.path} ({formatBytes(file.size)})
								</li>
							))}
						</ul>
					)}
					{files && files.length === 0 && (
						<p className="text-sm">No files in this torrent.</p>
					)}
				</div>
			)}
		</div>
	);
}
