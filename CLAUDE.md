We are writing fastscription, an application written in rust with https://fastframe.dev/moving-apps/ that would be used to transcript a conversation in realtime.

- This application will acts as a frontend for voxtype.
- Use will be able to select the audio source from which he wants to transcript the conversation, the main sound backend will be pipewire/pulseaudio in Linux.
- One of the goals is that the user will be able to see in realtime the transcript of the meeting while attend the meeting.
- The purpose is to help users that does not have good english listening skills, to understand the conversation.
- the application will only transcript to text the audio source the user has selected.
- The application will have a conversation history, users can group conversation, give a custom name, add tags to conversations, export conversations to several text formats.
- Application will be able to also start/stop voxtype systemd user service.

# IMPORTANT 
- You will not include claude or yourself as a co-author.
- You will not include any information related to the claude session in commits.
- Is important that git commits will not contains any information that can led to identify claude as coding platform.
