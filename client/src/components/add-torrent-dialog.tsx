import { useState } from "react";
import { useForm } from "react-hook-form";
import { zodResolver } from "@hookform/resolvers/zod";
import { z } from "zod";
import { trpc } from "../pages/trpc";
import { Button } from "./ui/button";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
	DialogTrigger,
} from "./ui/dialog";
import { Input } from "./ui/input";
import {
	Form,
	FormControl,
	FormDescription,
	FormField,
	FormItem,
	FormLabel,
	FormMessage,
} from "./ui/form";
import { Plus, Loader2 } from "lucide-react";
import { Textarea } from "./ui/textarea";

const FormSchema = z.object({
	magnetUris: z
		.string()
		.min(1, "At least one magnet URI is required")
		.refine((value) => {
			const uris = value
				.split("\n")
				.map((uri) => uri.trim())
				.filter((uri) => uri.length > 0);
			return uris.length > 0 && uris.every((uri) => uri.startsWith("magnet:"));
		}, "Please enter valid magnet URIs (starting with 'magnet:')"),
	category: z.string().optional(),
});

type FormData = z.infer<typeof FormSchema>;

export default function AddTorrentDialog() {
	const [isOpen, setIsOpen] = useState(false);
	const form = useForm<FormData>({
		resolver: zodResolver(FormSchema),
		defaultValues: {
			magnetUris: "",
			category: "",
		},
	});

	const utils = trpc.useUtils();
	const addTorrentMutation = trpc.add_torrent.useMutation({
		onSuccess: () => {
			setIsOpen(false);
			form.reset();
			utils.get_torrents.invalidate();
		},
	});

	const onSubmit = (data: FormData) => {
		const uris = data.magnetUris
			.split("\n")
			.map((uri) => uri.trim())
			.filter((uri) => uri.length > 0);

		addTorrentMutation.mutate({
			magnet_uris: uris,
			category: data.category?.trim() || null,
		});
	};

	return (
		<Dialog open={isOpen} onOpenChange={setIsOpen}>
			<DialogTrigger asChild>
				<Button>
					<Plus className="h-4 w-4 mr-2" />
					Add Torrent
				</Button>
			</DialogTrigger>
			<DialogContent className="sm:max-w-md">
				<DialogHeader>
					<DialogTitle>Add Torrent</DialogTitle>
				</DialogHeader>
				<Form {...form}>
					<form onSubmit={form.handleSubmit(onSubmit)} className="space-y-4">
						<FormField
							control={form.control}
							name="magnetUris"
							render={({ field }) => (
								<FormItem>
									<FormLabel>Magnet URIs</FormLabel>
									<FormControl>
										<Textarea
											className="whitespace-pre wrap-normal overflow-x-scroll"
											placeholder="magnet:?xt=urn:btih:...&#10;magnet:?xt=urn:btih:..."
											{...field}
										/>
									</FormControl>
									<FormDescription>
										Enter one or more magnet URIs, one per line
									</FormDescription>
									<FormMessage />
								</FormItem>
							)}
						/>
						<FormField
							control={form.control}
							name="category"
							render={({ field }) => (
								<FormItem>
									<FormLabel>Category (optional)</FormLabel>
									<FormControl>
										<Input placeholder="movies, tv, music, etc." {...field} />
									</FormControl>
									<FormMessage />
								</FormItem>
							)}
						/>
						<div className="flex gap-2 justify-end">
							<Button
								type="button"
								variant="outline"
								onClick={() => setIsOpen(false)}
							>
								Cancel
							</Button>
							<Button type="submit" disabled={addTorrentMutation.isPending}>
								{addTorrentMutation.isPending && (
									<Loader2 className="h-4 w-4 mr-2 animate-spin" />
								)}
								Add Torrent
								{form
									.watch("magnetUris")
									.split("\n")
									.filter((u) => u.trim()).length > 1
									? "s"
									: ""}
							</Button>
						</div>
						{addTorrentMutation.error && (
							<p className="text-sm text-red-400">
								{addTorrentMutation.error.message}
							</p>
						)}
					</form>
				</Form>
			</DialogContent>
		</Dialog>
	);
}
