-botName = @FoxBot
-creatorName = @Syfaro

# General Onboarding
welcome =
    Hi, I'm { -botName }.
    
    If you use me as an inline bot, I'll mirror content from many furry sites. When I post the image I'll include a direct link and a source link, if available. You can add your Twitter account with /twitter to get content from locked accounts you follow.
    
    If you send me an image, I'll try to find the source on FA.
    
    Add me to your group for features like /mirror (where I mirror all the links in a message, including messages you reply to) or /source (where I attempt to find the source of an image you're replying to).
    
    If I'm given edit permissions in your channel, I'll automatically edit posts to include a source link.
    
    Contact my creator { -creatorName } if you have any issues or feature suggestions.

welcome-group =
    Hi, I'm { -botName }.
    
    I'm here to help with sharing images! If you use me as an inline bot, I can easily get images from most furry sites, while keeping a link to the original source.
    
    I've also got a few commands to help in groups:
    · /mirror - I'll look at all the links in your message or the message you're replying to and mirror them
    · /source - I'll attempt to find if the photo you're replying to has been posted on FurAffinity
    
    You can also enable automatically finding sources for images posted in here with the /groupsource command. However, I must be an administrator in the group for this to work and it can only be enabled by an administrator.

welcome-try-me = Try Me!

# Inline Keyboard
inline-direct = Direct Link
inline-source = Source

# No Inline Results
inline-no-results-title = No results found
inline-no-results-body = I could not find any results for the provided query.

# Inline Videos
video-starting = Starting to process video...
video-too-large = Video was too large, aborting.
video-finished = Finished transcoding, uploading video...
video-return-button = Return and send

# Reverse Search
reverse-no-results = I was unable to find anything, sorry.
reverse-good-result = I found this (distance of { $distance }): { $link }
reverse-bad-result = I found this but it may not be the same image, be warned (distance of { $distance }): { $link }

# Twitter Onboarding
twitter-oob = Please follow the link and enter the 6 digit code returned: { $link }
twitter-welcome = Welcome aboard, { $userName }!
twitter-private = Let's do this in a private chat.

# In-group mirroring
mirror-no-links = Sorry, I could not find any links to mirror images from.
mirror-no-results = Sorry, I could not get any images from this message.
mirror-missing =
    In addition to these images, I could not fetch images from the following links:
    
    { $links }
    
    Sorry about that.

# In-group sourcing
source-no-photo = Sorry, I can't seem to find the photo here.

# In-group alternates
alternate-title = Here are some possible alternate versions:
alternate-posted-by = Posted by { $name }
alternate-distance = · { $link } (distance of { $distance })
alternate-multiple-photo = I can only find alternates for a single photo, sorry.

# Automatic group sourcing
automatic-single = It looks like this image may have come from here: { $link }
automatic-multiple = I found a few places this image may have come from:
automatic-multiple-result = · { $link } (distance of { $distance })
automatic-enable-not-admin = Sorry, you must be a group admin to enable this.
automatic-enable-bot-not-admin = Sorry, you must give me admin permissions due to a Telegram limitation.
automatic-enable-success = Automatic group sourcing is now enabled!
automatic-disable = This feature is now turned off.
automatic-enable-not-group = This feature is only supported in groups.
automatic-preview-disable = Sourced image previews disabled.
automatic-preview-enable = Sourced image previews enabled.

# Error Messages
error-generic = Oh no, something went wrong! Please send a message to my creator, { -creatorName }, saying what happened.
error-generic-count = Oh no, something went wrong! I've encountered { $count } errors. Please send a message to my creator, { -creatorName }, saying what happened.
error-uuid = Oh no, something went wrong! Please reply to this message saying what happened. You may also send a message to my creator, { -creatorName }, with this ID if you continue having issues: { $uuid }
error-uuid-count = Oh no, something went wrong! I've encountered { $count } errors. Please reply to this message saying what happened. You may also send a message to my creator, { -creatorName }, with this ID if you continue having issues: { $uuid }
error-feedback = Thank you for the feedback, hopefully we can get this issue resolved soon.

# Settings
settings-main = Let's take a look at some settings.
settings-site-order =
    Set the order of sites in returned results.
    
    This is especially useful for places where only one result is returned.
settings-name = If I should say the name of the site instead of just 'Source'
settings-name-toggled = Toggled using site name
settings-name-source = Only show Source on keyboard
settings-name-site = Show site name on keyboard
settings-unsupported = Sorry, not yet supported.
settings-move-unable = Unable to move { $name } to that position
settings-move-updated = Updated position for { $name }
settings-site-preference = Site Preference
settings-source-name = Source Name
