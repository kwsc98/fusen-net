# SPDX-License-Identifier: Apache-2.0 OR MIT

module DocsMetadata
  class Error < StandardError; end

  HEADER_LINE_LIMIT = 20

  module_function

  def read(contents, labels)
    block = metadata_block(contents)
    labels.to_h do |label|
      flexible_label = label.each_char.map { |character| Regexp.escape(character) }.join("\\s*")
      values = block.scan(/#{flexible_label}\s*[：:]\s*([^；;。*]+)/).flatten.map(&:strip)
      raise Error, "duplicate #{label} metadata near the top" if values.length > 1
      [label, values.first]
    end
  end

  def metadata_block(contents)
    lines = contents.lines.first(HEADER_LINE_LIMIT)
    raise Error, "document must start with one H1 heading" unless lines.first&.match?(/^# [^#]/)

    index = 1
    index += 1 while index < lines.length && lines[index].strip.empty?
    first = lines[index]
    raise Error, "missing visible metadata block immediately after the H1" unless first

    mode = if first.match?(/^\s*>/)
             :quote
           elsif first.match?(/^\s*-\s+/)
             :list
           end
    raise Error, "metadata must be a visible blockquote or list immediately after the H1" unless mode

    block_lines = []
    while index < lines.length
      line = lines[index]
      matches_mode = if mode == :quote
                       line.match?(/^\s*>/)
                     else
                       line.match?(/^\s*-\s+/) || line.match?(/^\s{2,}\S/)
                     end
      break unless matches_mode
      block_lines << line
      index += 1
    end

    raw = block_lines.join
    if raw.match?(/<!--|-->|```|~~~|<\/?[A-Za-z][^>]*>/)
      raise Error, "metadata block may not contain raw HTML, HTML comments, or code fences"
    end

    normalized = block_lines.map do |line|
      if mode == :quote
        line.sub(/^\s*>\s?/, "").strip
      elsif line.match?(/^\s*-\s+/)
        "#{line.sub(/^\s*-\s+/, '').strip}；"
      else
        line.strip
      end
    end.join(" ")
    normalized.gsub("**", " ").gsub(/\s+/, " ")
  end
end
