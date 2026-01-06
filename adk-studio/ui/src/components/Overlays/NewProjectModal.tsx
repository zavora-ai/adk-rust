import { useState } from 'react';

interface Props {
    onConfirm: (name: string) => void;
    onClose: () => void;
}

export function NewProjectModal({ onConfirm, onClose }: Props) {
    const [name, setName] = useState('');

    const handleSubmit = (e: React.FormEvent) => {
        e.preventDefault();
        if (name.trim()) {
            onConfirm(name.trim());
        }
    };

    return (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={onClose}>
            <div className="bg-studio-panel rounded-lg w-96 flex flex-col border border-gray-700 shadow-xl" onClick={e => e.stopPropagation()}>
                <div className="flex justify-between items-center p-4 border-b border-gray-700">
                    <h2 className="text-lg font-semibold text-white">New Project</h2>
                    <button onClick={onClose} className="text-gray-400 hover:text-white text-xl">×</button>
                </div>
                <form onSubmit={handleSubmit} className="p-4">
                    <div className="mb-4">
                        <label className="block text-sm text-gray-400 mb-1">Project Name</label>
                        <input
                            autoFocus
                            type="text"
                            value={name}
                            onChange={e => setName(e.target.value)}
                            className="w-full px-3 py-2 bg-studio-bg border border-gray-600 rounded text-sm text-white focus:border-blue-500 focus:outline-none"
                            placeholder="My Awesome Agent"
                        />
                    </div>
                    <div className="flex justify-end gap-2">
                        <button type="button" onClick={onClose} className="px-3 py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm text-white">Cancel</button>
                        <button type="submit" disabled={!name.trim()} className="px-3 py-2 bg-blue-600 hover:bg-blue-500 rounded text-sm text-white disabled:opacity-50 disabled:cursor-not-allowed">Create Project</button>
                    </div>
                </form>
            </div>
        </div>
    );
}
